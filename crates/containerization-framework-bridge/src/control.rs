//! How a second caller reaches a session this process owns.
//!
//! The VM dies with the process that created it, so a joining `run`, `exec` or
//! `shell` connects to a socket in the session's state directory and the owning
//! process runs the command on its behalf.
//!
//! What crosses is the client's **stdio**, not its bytes: the request carries
//! the client's descriptors via `SCM_RIGHTS` — its tty, or its stdin, stdout
//! and stderr when it has none — and the owner hands them straight to the guest
//! process. Nothing relays keystrokes, and the owner's code path is the same as
//! for its own.
//!
//! The socket carries only what a descriptor cannot: the request, a nudge per
//! window resize, and the exit code back.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Socket file name, inside the container's runtime directory.
pub const CONTROL_SOCKET: &str = "control.sock";

/// Separates list elements within a request line; cannot occur in an argument
/// or environment variable.
const UNIT: char = '\u{1f}';

/// Written by the client whenever its window changes size.
const RESIZE: u8 = b'R';

/// Enough for any plausible argv and environment; larger is a bug.
const MAX_REQUEST: usize = 64 * 1024;

/// A stream the caller leaves unattached; the guest neither reads nor writes it.
pub const UNATTACHED: RawFd = -1;

/// The descriptors a guest process runs against, each the caller's own: its
/// terminal, which the guest reads and sizes itself against, and whichever of
/// stdin, stdout and stderr the caller attached. Whatever the shell redirected
/// stays redirected, because each stream is written where the caller's own
/// descriptor points.
///
/// Descriptors cross in the order the labels list them, so both ends agree on
/// which is which without sending numbers that mean nothing to the other side.
#[derive(Debug, Eq, PartialEq)]
pub struct Stdio {
  pub terminal: RawFd,
  pub stdin: RawFd,
  pub stdout: RawFd,
  pub stderr: RawFd,
}

impl Stdio {
  /// A terminal the guest reads, writing what it has to say to `stdout`.
  ///
  /// Its stderr arrives there too. One pty carries every stream a process on a
  /// terminal writes, so by the time the bytes leave the guest nothing
  /// distinguishes them.
  pub fn terminal(terminal: RawFd, stdout: RawFd) -> Self {
    Self {
      terminal,
      stdin: UNATTACHED,
      stdout,
      stderr: UNATTACHED,
    }
  }

  /// This process's own streams, leaving out stdin when the guest process is
  /// to read none.
  pub fn inherit(interactive: bool) -> Self {
    Self {
      terminal: UNATTACHED,
      stdin: if interactive { libc::STDIN_FILENO } else { UNATTACHED },
      stdout: libc::STDOUT_FILENO,
      stderr: libc::STDERR_FILENO,
    }
  }

  /// The same streams, on descriptors of their own.
  ///
  /// Whoever runs the guest process closes what it is handed, and a caller
  /// keeps its own streams open, so each side works from a duplicate.
  pub fn try_clone(&self) -> io::Result<Self> {
    self.mapped(crate::terminal::lend)
  }

  /// The same streams, each descriptor replaced by what `replace` makes of it.
  fn mapped<E>(&self, replace: impl Fn(RawFd) -> Result<RawFd, E>) -> Result<Self, E> {
    let mut mapped = Self {
      terminal: UNATTACHED,
      stdin: UNATTACHED,
      stdout: UNATTACHED,
      stderr: UNATTACHED,
    };

    for (label, descriptor) in self.attached() {
      mapped.set(label, replace(descriptor)?);
    }

    Ok(mapped)
  }

  fn labelled(&self) -> [(char, RawFd); 4] {
    [
      ('t', self.terminal),
      ('i', self.stdin),
      ('o', self.stdout),
      ('e', self.stderr),
    ]
  }

  fn set(&mut self, label: char, descriptor: RawFd) {
    match label {
      't' => self.terminal = descriptor,
      'i' => self.stdin = descriptor,
      'o' => self.stdout = descriptor,
      _ => self.stderr = descriptor,
    }
  }

  /// Only the streams the caller attached, in the order they cross.
  fn attached(&self) -> Vec<(char, RawFd)> {
    self
      .labelled()
      .into_iter()
      .filter(|(_, descriptor)| *descriptor != UNATTACHED)
      .collect()
  }

  fn descriptors(&self) -> Vec<RawFd> {
    self
      .attached()
      .into_iter()
      .map(|(_, descriptor)| descriptor)
      .collect()
  }

  fn labels(&self) -> String {
    self
      .attached()
      .into_iter()
      .map(|(label, _)| label)
      .collect()
  }

  /// Pairs the labels a request carried with the descriptors that came beside
  /// it. `None` when they disagree, or a label names no stream.
  fn from_labels(labels: &str, descriptors: &[RawFd]) -> Option<Self> {
    if labels.chars().count() != descriptors.len() || labels.chars().any(|label| !"tioe".contains(label)) {
      return None;
    }

    let mut stdio = Self {
      terminal: UNATTACHED,
      stdin: UNATTACHED,
      stdout: UNATTACHED,
      stderr: UNATTACHED,
    };

    for (label, descriptor) in labels.chars().zip(descriptors) {
      stdio.set(label, *descriptor);
    }

    Some(stdio)
  }
}

/// What a client asks the owner to run.
#[derive(Debug, Eq, PartialEq)]
pub struct Request {
  pub arguments: Vec<String>,
  pub environment: Vec<String>,
  /// `None` is the image's own user, and crosses as an empty line.
  pub user: Option<String>,
  pub working_directory: String,
}

impl Request {
  fn encode(&self, stdio: &Stdio) -> String {
    format!(
      "{}\n{}\n{}\n{}\n{}",
      self.arguments.join(&UNIT.to_string()),
      self.environment.join(&UNIT.to_string()),
      self.user.as_deref().unwrap_or(""),
      stdio.labels(),
      self.working_directory
    )
  }

  /// The request, and the streams its descriptors are. The working directory
  /// comes last: it is the only field that may itself contain a newline.
  fn decode(payload: &str, descriptors: &[RawFd]) -> Option<(Self, Stdio)> {
    let mut lines = payload.splitn(5, '\n');
    let arguments = lines.next()?;
    let environment = lines.next()?;
    let user = lines.next()?;
    let stdio = Stdio::from_labels(lines.next()?, descriptors)?;

    Some((
      Self {
        arguments: split(arguments),
        environment: split(environment),
        user: (!user.is_empty()).then(|| user.to_string()),
        working_directory: lines.next()?.to_string(),
      },
      stdio,
    ))
  }
}

/// Whether a connection sent nothing, i.e. was a liveness check.
///
/// A client always sends request and descriptors in one message, so neither
/// means it closed before speaking — not a failed attach worth reporting.
fn probe(payload: &str, descriptors: &[OwnedFd]) -> bool {
  payload.is_empty() && descriptors.is_empty()
}

fn split(line: &str) -> Vec<String> {
  if line.is_empty() {
    return Vec::new();
  }

  line.split(UNIT).map(str::to_string).collect()
}

/// Whether a live owner answers at this path.
///
/// The inode outlives its owner, so only a connect tells dead from listening.
pub fn served(path: &Path) -> bool {
  UnixStream::connect(path).is_ok()
}

/// Binds the control socket, creating its directory and replacing any socket
/// left by a dead owner (safe because `served` found nothing answering).
pub fn bind(path: &Path) -> io::Result<UnixListener> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  if path.exists() && !served(path) {
    std::fs::remove_file(path)?;
  }

  UnixListener::bind(path)
}

/// Serves requests until the listener is dropped, one thread per attached
/// client.
///
/// `run` gets the request and the client's stdio and returns the guest's exit
/// code. `resize` gets the client's terminal on each window change, and is
/// never called for a client that sent none.
pub fn serve<R, S>(listener: &UnixListener, run: R, resize: S)
where
  R: Fn(&Request, &Stdio, &str) -> i32 + Sync,
  S: Fn(&str, RawFd) + Sync,
{
  std::thread::scope(|scope| {
    for (sequence, stream) in listener.incoming().enumerate() {
      let Ok(stream) = stream else {
        // A failed accept says nothing about the next one.
        continue;
      };

      let run = &run;
      let resize = &resize;

      scope.spawn(move || {
        // Unique per attach, so a resize reaches only its own process.
        let id = format!("attach-{sequence}");

        if let Err(error) = attend(stream, &id, run, resize) {
          eprintln!("compostbin: a session client went away: {error}");
        }
      });
    }
  });
}

fn attend<R, S>(stream: UnixStream, id: &str, run: R, resize: S) -> io::Result<()>
where
  R: Fn(&Request, &Stdio, &str) -> i32,
  // The resize watcher gets its own thread so resizes arrive while the guest
  // is quiet.
  S: Fn(&str, RawFd) + Send,
{
  let (payload, received) = receive(&stream)?;

  // `served` (via `is_running`, `run`, `doctor`) connects and closes without
  // sending. Reporting it would print into the session on every check.
  if probe(&payload, &received) {
    return Ok(());
  }

  // Owned, so they close once the guest is done; the client keeps its own.
  let descriptors: Vec<RawFd> = received.iter().map(AsRawFd::as_raw_fd).collect();

  let (request, borrowed) = Request::decode(&payload, &descriptors)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed request"))?;

  // Duplicates the Swift side keeps (see `terminal::lend`). Handing over the
  // received fds let a later attach reuse a number while a previous reader was
  // still on it.
  let stdio = borrowed.try_clone()?;

  std::thread::scope(|scope| {
    let mut watching = stream.try_clone()?;
    let terminal = stdio.terminal;

    // Only a client on a terminal has a window to resize, and only it nudges.
    if terminal != UNATTACHED {
      scope.spawn(move || {
        let mut byte = [0u8; 1];

        // Ends when the client closes after getting its exit code, so this
        // thread cannot outlive the attach.
        while let Ok(1) = watching.read(&mut byte) {
          if byte[0] == RESIZE {
            resize(id, terminal);
          }
        }
      });
    }

    let code = run(&request, &stdio, id);

    (&stream).write_all(&code.to_be_bytes())?;
    // Unblocks the watcher above, which is otherwise still reading.
    let _ = stream.shutdown(std::net::Shutdown::Both);

    Ok(())
  })
}

/// Asks the owner to run something on this process's stdio; blocks until the
/// guest exits and returns its exit code. Nothing is relayed meanwhile.
///
/// `resized` returns whether this process's window changed size since last
/// asked. The owner cannot see that for itself, so each change crosses as a
/// nudge, and the owner resizes the guest's pty on our behalf.
pub fn request(path: &Path, request: &Request, stdio: &Stdio, resized: &(dyn Fn() -> bool + Sync)) -> io::Result<i32> {
  let stream = UnixStream::connect(path)?;

  send(&stream, request.encode(stdio).as_bytes(), &stdio.descriptors())?;

  let wait = || {
    let mut code = [0u8; 4];
    (&stream).read_exact(&mut code)?;

    Ok(i32::from_be_bytes(code))
  };

  // Only a caller with a terminal has a window that can change size.
  if stdio.terminal == UNATTACHED {
    return wait();
  }

  crate::terminal::while_resizing(
    resized,
    || {
      // A write that fails needs no handling: the owner is gone, and the read
      // below is about to say so.
      let _ = (&stream).write_all(&[RESIZE]);
    },
    wait,
  )
}

/// Where a container's control socket lives, given its directory.
pub fn socket_path(container_dir: &Path) -> PathBuf {
  container_dir.join(CONTROL_SOCKET)
}

/// `sendmsg` with the payload and the descriptors in one `SCM_RIGHTS` control
/// message.
fn send(stream: &UnixStream, payload: &[u8], descriptors: &[RawFd]) -> io::Result<()> {
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };
  let bytes = size_of_val(descriptors) as u32;

  // SAFETY: every pointer below refers to a local that outlives the call, and
  // the control buffer is sized by CMSG_SPACE for MAX_DESCRIPTORS of them.
  let sent = unsafe {
    let mut message: libc::msghdr = std::mem::zeroed();
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr() as *mut libc::c_void;
    // Exactly these descriptors' worth, not the buffer's capacity: any length
    // past it reads as a second, malformed control message — EINVAL.
    message.msg_controllen = libc::CMSG_SPACE(bytes);

    let header = libc::CMSG_FIRSTHDR(&message);
    (*header).cmsg_level = libc::SOL_SOCKET;
    (*header).cmsg_type = libc::SCM_RIGHTS;
    (*header).cmsg_len = libc::CMSG_LEN(bytes) as _;
    std::ptr::copy_nonoverlapping(
      descriptors.as_ptr(),
      libc::CMSG_DATA(header) as *mut RawFd,
      descriptors.len(),
    );

    libc::sendmsg(stream.as_raw_fd(), &message, 0)
  };

  if sent < 0 {
    return Err(io::Error::last_os_error());
  }

  Ok(())
}

/// The counterpart of `send`: the payload, and whichever descriptors came, in
/// the order they were sent.
fn receive(stream: &UnixStream) -> io::Result<(String, Vec<OwnedFd>)> {
  let mut payload = vec![0u8; MAX_REQUEST];
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_mut_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };

  // SAFETY: as in `send`. The descriptors the kernel writes into the control
  // buffer are ours to own from the moment recvmsg returns, and it writes no
  // more of them than the buffer holds.
  let (read, descriptors) = unsafe {
    let mut message: libc::msghdr = std::mem::zeroed();
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr() as *mut libc::c_void;
    message.msg_controllen = space.len() as libc::socklen_t;

    let read = libc::recvmsg(stream.as_raw_fd(), &mut message, 0);
    if read < 0 {
      return Err(io::Error::last_os_error());
    }

    let header = libc::CMSG_FIRSTHDR(&message);
    let mut descriptors = Vec::new();

    if !header.is_null() && (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
      let data = libc::CMSG_DATA(header) as *const RawFd;
      let count = ((*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / size_of::<RawFd>();

      for index in 0..count {
        descriptors.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(index))));
      }
    }

    (read as usize, descriptors)
  };

  payload.truncate(read);

  Ok((String::from_utf8_lossy(&payload).into_owned(), descriptors))
}

/// A terminal, or stdin, stdout and stderr.
const MAX_DESCRIPTORS: usize = 3;

/// Buffer capacity for one `SCM_RIGHTS` message carrying `MAX_DESCRIPTORS`.
/// `CMSG_SPACE` isn't const, so the exact length is computed at the call; this
/// only has to be at least that.
const CMSG_SPACE: usize = 64;

const _: () = assert!(CMSG_SPACE >= 32 + MAX_DESCRIPTORS * size_of::<RawFd>());

#[cfg(test)]
mod tests {
  use super::*;

  fn request() -> Request {
    Request {
      arguments: vec!["bash".to_string()],
      environment: vec!["IS_SANDBOX=1".to_string(), "TERM=xterm".to_string()],
      user: Some("root".to_string()),
      working_directory: "/workspace/compostbin".to_string(),
    }
  }

  /// Descriptors as the owner receives them: numbers of its own, in the order
  /// the client's labels name.
  fn received(stdio: &Stdio) -> Vec<RawFd> {
    (10..).take(stdio.descriptors().len()).collect()
  }

  fn round_trip(stdio: &Stdio) -> Option<(Request, Stdio)> {
    Request::decode(&request().encode(stdio), &received(stdio))
  }

  #[test]
  fn round_trips_a_request_on_a_terminal() {
    let (decoded, stdio) = round_trip(&Stdio::terminal(7, 8)).expect("decode");

    assert_eq!(decoded, request());
    assert_eq!(
      stdio,
      Stdio::terminal(10, 11),
      "the owner's numbers for the terminal and the caller's stdout"
    );
  }

  #[test]
  fn round_trips_a_request_on_a_callers_own_streams() {
    let (decoded, stdio) = round_trip(&Stdio::inherit(true)).expect("decode");

    assert_eq!(decoded, request());
    assert_eq!(
      stdio,
      Stdio {
        terminal: UNATTACHED,
        stdin: 10,
        stdout: 11,
        stderr: 12,
      },
      "each stream in the order its label crossed"
    );
  }

  #[test]
  fn round_trips_a_request_whose_process_reads_no_input() {
    let (_, stdio) = round_trip(&Stdio::inherit(false)).expect("decode");

    assert_eq!(stdio.stdin, UNATTACHED);
    assert_eq!((stdio.stdout, stdio.stderr), (10, 11));
  }

  #[test]
  fn round_trips_a_request_with_nothing_in_its_lists() {
    let empty = Request {
      arguments: Vec::new(),
      environment: Vec::new(),
      user: None,
      working_directory: "/".to_string(),
    };
    let stdio = Stdio::terminal(7, 8);

    assert_eq!(
      Request::decode(&empty.encode(&stdio), &received(&stdio)),
      Some((empty, Stdio::terminal(10, 11)))
    );
  }

  #[test]
  fn reads_nothing_from_a_truncated_request() {
    assert_eq!(Request::decode("bash", &[10]), None);
    assert_eq!(Request::decode("bash\nIS_SANDBOX=1\nroot\nt", &[10]), None);
  }

  /// Both ends would disagree about which stream is which.
  #[test]
  fn reads_nothing_from_a_request_whose_labels_and_descriptors_disagree() {
    let stdio = Stdio::inherit(true);
    let payload = request().encode(&stdio);

    assert_eq!(Request::decode(&payload, &[10, 11]), None, "one descriptor short");
    assert_eq!(
      Request::decode(&request().encode(&Stdio::terminal(7, 8)), &[10, 11, 12]),
      None,
      "one label short"
    );
  }

  #[test]
  fn reads_nothing_from_a_request_naming_a_stream_that_does_not_exist() {
    assert_eq!(Request::decode("bash\n\n\nx\n/", &[10]), None);
  }

  /// `served` connects and closes; that must not be reported as malformed.
  #[test]
  fn a_connection_that_says_nothing_is_a_liveness_probe() {
    assert!(probe("", &[]));
  }

  #[test]
  fn a_connection_that_says_something_is_not_a_probe() {
    let file = tempfile::tempfile().expect("a temp file");

    assert!(!probe(&request().encode(&Stdio::terminal(7, 8)), &[]));
    assert!(!probe("", &[OwnedFd::from(file)]));
  }

  #[test]
  fn clones_every_attached_stream_onto_a_descriptor_of_its_own() {
    let stdio = Stdio::inherit(true).try_clone().expect("clone");

    assert_eq!(stdio.terminal, UNATTACHED);

    for descriptor in [stdio.stdin, stdio.stdout, stdio.stderr] {
      assert!(
        descriptor > libc::STDERR_FILENO,
        "a duplicate, so closing it leaves this process's own stream open"
      );

      // SAFETY: a descriptor this test duplicated, closed once.
      unsafe { libc::close(descriptor) };
    }
  }

  #[test]
  fn clones_nothing_for_a_stream_the_caller_left_out() {
    let stdio = Stdio::inherit(false).try_clone().expect("clone");

    assert_eq!(stdio.stdin, UNATTACHED, "a process that reads no input");
    assert!(stdio.stdout > libc::STDERR_FILENO);

    for descriptor in [stdio.stdout, stdio.stderr] {
      // SAFETY: as above.
      unsafe { libc::close(descriptor) };
    }
  }

  #[test]
  fn says_nothing_is_served_at_a_path_with_no_socket() {
    let directory = tempfile::tempdir().expect("a temp dir");

    assert!(!served(&socket_path(directory.path())));
  }

  /// Every stream a caller without a terminal sends, which is the most of them.
  #[test]
  fn carries_three_descriptors_and_a_payload_across() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let path = socket_path(directory.path());
    let listener = bind(&path).expect("bind");
    let stdio = Stdio::inherit(true);

    let sending = std::thread::spawn({
      let payload = request().encode(&stdio);
      let descriptors = stdio.descriptors();

      move || {
        let client = UnixStream::connect(&path).expect("connect");
        send(&client, payload.as_bytes(), &descriptors).expect("send");
      }
    });

    let (stream, _) = listener.accept().expect("accept");
    let (payload, descriptors) = receive(&stream).expect("receive");

    sending.join().expect("the sender should finish");

    assert_eq!(descriptors.len(), MAX_DESCRIPTORS, "stdin, stdout and stderr");

    let numbers: Vec<RawFd> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
    let (decoded, received) = Request::decode(&payload, &numbers).expect("decode");

    assert_eq!(decoded, request());
    assert_eq!(
      (received.stdin, received.stdout, received.stderr),
      (numbers[0], numbers[1], numbers[2])
    );
  }

  #[test]
  fn rebinds_over_a_socket_whose_owner_is_gone() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let path = socket_path(directory.path());

    drop(bind(&path).expect("the first bind"));

    assert!(path.exists(), "the inode outlives the listener");
    assert!(bind(&path).is_ok(), "a dead socket should not block the next owner");
  }
}
