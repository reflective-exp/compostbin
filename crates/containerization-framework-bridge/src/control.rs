//! How a second terminal reaches a session this process owns.
//!
//! The VM dies with the process that created it, so a joining `run` or `shell`
//! connects to a socket in the session's state directory and the owning process
//! runs the command on its behalf.
//!
//! What crosses is the client's **terminal**, not its bytes: the request
//! carries the client's tty via `SCM_RIGHTS`, and the owner hands it straight
//! to the guest process as stdio. Nothing relays keystrokes, and the owner's
//! code path is the same as for its own terminal.
//!
//! The socket carries only what a descriptor cannot: the request, a nudge per
//! window resize, and the exit code back.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Socket file name, inside the container's runtime directory.
pub const CONTROL_SOCKET: &str = "control.sock";

/// Separates list elements within a request line; cannot occur in an argument
/// or environment variable.
const UNIT: char = '\u{1f}';

/// Written by the client whenever its window changes size.
const RESIZE: u8 = b'R';

/// Enough for any plausible argv and environment; larger is a bug.
const MAX_REQUEST: usize = 64 * 1024;

/// How often the client checks for a window resize. Also the longest an attach
/// can take to end.
const RESIZE_POLL: Duration = Duration::from_millis(100);

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
  fn encode(&self) -> String {
    format!(
      "{}\n{}\n{}\n{}",
      self.arguments.join(&UNIT.to_string()),
      self.environment.join(&UNIT.to_string()),
      self.user.as_deref().unwrap_or(""),
      self.working_directory
    )
  }

  fn decode(payload: &str) -> Option<Self> {
    let mut lines = payload.splitn(4, '\n');
    let arguments = lines.next()?;
    let environment = lines.next()?;
    let user = lines.next()?;

    Some(Self {
      arguments: split(arguments),
      environment: split(environment),
      user: (!user.is_empty()).then(|| user.to_string()),
      working_directory: lines.next()?.to_string(),
    })
  }
}

/// Whether a connection sent nothing, i.e. was a liveness check.
///
/// A client always sends request and terminal in one message, so neither means
/// it closed before speaking — not a failed attach worth reporting.
fn probe(payload: &str, terminal: &Option<OwnedFd>) -> bool {
  payload.is_empty() && terminal.is_none()
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
/// terminal.
///
/// `run` gets the request and the client's terminal and returns the guest's
/// exit code. `resize` gets that terminal on each client window change.
pub fn serve<R, S>(listener: &UnixListener, run: R, resize: S)
where
  R: Fn(&Request, RawFd, &str) -> i32 + Sync,
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
  R: Fn(&Request, RawFd, &str) -> i32,
  // The resize watcher gets its own thread so resizes arrive while the guest
  // is quiet.
  S: Fn(&str, RawFd) + Send,
{
  let (payload, terminal) = receive(&stream)?;

  // `served` (via `is_running`, `run`, `doctor`) connects and closes without
  // sending. Reporting it would print into the session on every check.
  if probe(&payload, &terminal) {
    return Ok(());
  }

  let request =
    Request::decode(&payload).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed request"))?;

  // Owned so it closes once the guest is done; the client keeps its own copy.
  let terminal = terminal.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no terminal was passed"))?;

  // A duplicate the Swift side keeps (see `terminal::lend`). Handing over the
  // received fd let a later attach reuse its number while a previous reader
  // was still on it.
  let raw = crate::terminal::lend(terminal.as_raw_fd())?;

  std::thread::scope(|scope| {
    let mut watching = stream.try_clone()?;

    scope.spawn(move || {
      let mut byte = [0u8; 1];

      // Ends when the client closes after getting its exit code, so this
      // thread cannot outlive the attach.
      while let Ok(1) = watching.read(&mut byte) {
        if byte[0] == RESIZE {
          resize(id, raw);
        }
      }
    });

    let code = run(&request, raw, id);

    (&stream).write_all(&code.to_be_bytes())?;
    // Unblocks the watcher above, which is otherwise still reading.
    let _ = stream.shutdown(std::net::Shutdown::Both);

    Ok(())
  })
}

/// Asks the owner to run something on this process's terminal; blocks until
/// the guest exits and returns its exit code. Nothing is relayed meanwhile.
///
/// `resized` returns whether the terminal changed size since last asked. It is
/// polled, not signal-driven, so nothing here must be async-signal-safe.
pub fn request(
  path: &Path,
  request: &Request,
  terminal: RawFd,
  resized: &(dyn Fn() -> bool + Sync),
) -> io::Result<i32> {
  let stream = UnixStream::connect(path)?;

  send(&stream, request.encode().as_bytes(), terminal)?;

  let attached = AtomicBool::new(true);

  std::thread::scope(|scope| {
    let mut nudging = stream.try_clone()?;
    let attached = &attached;

    scope.spawn(move || {
      // Polled so clearing the flag ends this thread within one interval. A
      // watcher woken only by resizes would keep `scope` (and the process)
      // hanging after the guest exits.
      while attached.load(Ordering::Relaxed) {
        if resized() && nudging.write_all(&[RESIZE]).is_err() {
          return;
        }

        std::thread::sleep(RESIZE_POLL);
      }
    });

    let mut code = [0u8; 4];
    let read = (&stream).read_exact(&mut code);

    // Before `scope` joins, even on error, or an owner that died without
    // answering would strand us.
    attached.store(false, Ordering::Relaxed);
    read?;

    Ok(i32::from_be_bytes(code))
  })
}

/// Where a container's control socket lives, given its directory.
pub fn socket_path(container_dir: &Path) -> PathBuf {
  container_dir.join(CONTROL_SOCKET)
}

/// `sendmsg` with the payload and one descriptor in a `SCM_RIGHTS` control
/// message.
fn send(stream: &UnixStream, payload: &[u8], descriptor: RawFd) -> io::Result<()> {
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };

  // SAFETY: every pointer below refers to a local that outlives the call, and
  // the control buffer is sized by CMSG_SPACE for exactly one descriptor.
  let sent = unsafe {
    let mut message: libc::msghdr = std::mem::zeroed();
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr() as *mut libc::c_void;
    // Exactly one descriptor's worth, not the buffer's capacity: any length
    // past it reads as a second, malformed control message — EINVAL.
    message.msg_controllen = libc::CMSG_SPACE(size_of::<RawFd>() as u32);

    let header = libc::CMSG_FIRSTHDR(&message);
    (*header).cmsg_level = libc::SOL_SOCKET;
    (*header).cmsg_type = libc::SCM_RIGHTS;
    (*header).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as u32) as _;
    std::ptr::write_unaligned(libc::CMSG_DATA(header) as *mut RawFd, descriptor);

    libc::sendmsg(stream.as_raw_fd(), &message, 0)
  };

  if sent < 0 {
    return Err(io::Error::last_os_error());
  }

  Ok(())
}

/// The counterpart of `send`: the payload, and the descriptor if one came.
fn receive(stream: &UnixStream) -> io::Result<(String, Option<OwnedFd>)> {
  let mut payload = vec![0u8; MAX_REQUEST];
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_mut_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };

  // SAFETY: as in `send`. The descriptor the kernel writes into the control
  // buffer is ours to own from the moment recvmsg returns.
  let (read, descriptor) = unsafe {
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
    let descriptor =
      if header.is_null() || (*header).cmsg_level != libc::SOL_SOCKET || (*header).cmsg_type != libc::SCM_RIGHTS {
        None
      } else {
        Some(OwnedFd::from_raw_fd(std::ptr::read_unaligned(
          libc::CMSG_DATA(header) as *const RawFd
        )))
      };

    (read as usize, descriptor)
  };

  payload.truncate(read);

  Ok((String::from_utf8_lossy(&payload).into_owned(), descriptor))
}

/// Buffer capacity for one `SCM_RIGHTS` message. `CMSG_SPACE` isn't const, so
/// the exact length is computed at the call; this only has to be at least that.
const CMSG_SPACE: usize = 64;

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

  #[test]
  fn round_trips_a_request() {
    assert_eq!(Request::decode(&request().encode()), Some(request()));
  }

  #[test]
  fn round_trips_a_request_with_nothing_in_its_lists() {
    let empty = Request {
      arguments: Vec::new(),
      environment: Vec::new(),
      user: None,
      working_directory: "/".to_string(),
    };

    assert_eq!(Request::decode(&empty.encode()), Some(empty));
  }

  #[test]
  fn reads_nothing_from_a_truncated_request() {
    assert_eq!(Request::decode("bash"), None);
    assert_eq!(Request::decode("bash\nIS_SANDBOX=1\n/workspace"), None);
  }

  /// `served` connects and closes; that must not be reported as malformed.
  #[test]
  fn a_connection_that_says_nothing_is_a_liveness_probe() {
    assert!(probe("", &None));
  }

  #[test]
  fn a_connection_that_says_something_is_not_a_probe() {
    let file = tempfile::tempfile().expect("a temp file");

    assert!(!probe(&request().encode(), &None));
    assert!(!probe("", &Some(OwnedFd::from(file))));
  }

  #[test]
  fn says_nothing_is_served_at_a_path_with_no_socket() {
    let directory = tempfile::tempdir().expect("a temp dir");

    assert!(!served(&socket_path(directory.path())));
  }

  #[test]
  fn carries_a_descriptor_and_a_payload_across() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let path = socket_path(directory.path());
    let listener = bind(&path).expect("bind");

    let sending = std::thread::spawn(move || {
      let client = UnixStream::connect(&path).expect("connect");
      let file = tempfile::tempfile().expect("a temp file");
      send(&client, request().encode().as_bytes(), file.as_raw_fd()).expect("send");
    });

    let (stream, _) = listener.accept().expect("accept");
    let (payload, descriptor) = receive(&stream).expect("receive");

    sending.join().expect("the sender should finish");

    assert_eq!(Request::decode(&payload), Some(request()));
    assert!(descriptor.is_some(), "a descriptor should have come across");
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
