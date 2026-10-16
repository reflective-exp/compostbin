//! Clients pass their stdio descriptors to the owning process via `SCM_RIGHTS`;
//! the socket itself carries only the request, resize nudges and the exit code.

use super::stdio::{Stdio, UNATTACHED};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

pub const CONTROL_SOCKET: &str = "control.sock";

/// Cannot occur in an argument or environment variable.
const UNIT: char = '\u{1f}';

const RESIZE: u8 = b'R';

const MAX_REQUEST: usize = 64 * 1024;

/// Descriptors cross in this order.
fn labelled(stdio: &Stdio) -> [(char, RawFd); MAX_DESCRIPTORS] {
  [
    ('t', stdio.terminal),
    ('i', stdio.stdin),
    ('o', stdio.stdout),
    ('e', stdio.stderr),
  ]
}

fn set(stdio: &mut Stdio, label: char, descriptor: RawFd) -> Option<()> {
  let stream = match label {
    't' => &mut stdio.terminal,
    'i' => &mut stdio.stdin,
    'o' => &mut stdio.stdout,
    'e' => &mut stdio.stderr,
    _ => return None,
  };

  *stream = descriptor;

  Some(())
}

fn attached(stdio: &Stdio) -> impl Iterator<Item = (char, RawFd)> {
  labelled(stdio)
    .into_iter()
    .filter(|(_, descriptor)| *descriptor != UNATTACHED)
}

fn descriptors(stdio: &Stdio) -> Vec<RawFd> {
  attached(stdio).map(|(_, descriptor)| descriptor).collect()
}

fn labels(stdio: &Stdio) -> String {
  attached(stdio).map(|(label, _)| label).collect()
}

fn from_labels(labels: &str, descriptors: &[RawFd]) -> Option<Stdio> {
  if labels.chars().count() != descriptors.len() {
    return None;
  }

  let mut stdio = Stdio::nothing();

  for (label, descriptor) in labels.chars().zip(descriptors) {
    set(&mut stdio, label, *descriptor)?;
  }

  Some(stdio)
}

#[derive(Debug, Eq, PartialEq)]
pub struct Request {
  pub arguments: Vec<String>,
  pub environment: Vec<String>,
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
      labels(stdio),
      self.working_directory
    )
  }

  /// The working directory comes last because it alone may contain a newline.
  fn decode(payload: &str, descriptors: &[RawFd]) -> Option<(Self, Stdio)> {
    let mut lines = payload.splitn(5, '\n');
    let arguments = lines.next()?;
    let environment = lines.next()?;
    let user = lines.next()?;
    let stdio = from_labels(lines.next()?, descriptors)?;

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

fn probe(payload: &str, descriptors: &[OwnedFd]) -> bool {
  payload.is_empty() && descriptors.is_empty()
}

fn split(line: &str) -> Vec<String> {
  if line.is_empty() {
    return Vec::new();
  }

  line.split(UNIT).map(str::to_string).collect()
}

/// The socket file outlives its owner, so only a connect tells dead from listening.
pub fn served(path: &Path) -> bool {
  UnixStream::connect(path).is_ok()
}

/// Replaces any socket left by a dead owner.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }

  if path.exists() && !served(path) {
    std::fs::remove_file(path)?;
  }

  UnixListener::bind(path)
}

pub fn serve<R, S, F>(listener: &UnixListener, run: R, resize: S, failed: F)
where
  R: Fn(&Request, &Stdio, &str) -> i32 + Sync,
  S: Fn(&str, RawFd) + Sync,
  F: Fn(io::Error) + Sync,
{
  std::thread::scope(|scope| {
    for (sequence, stream) in listener.incoming().enumerate() {
      let Ok(stream) = stream else {
        continue;
      };

      let run = &run;
      let resize = &resize;
      let failed = &failed;

      scope.spawn(move || {
        let id = format!("attach-{sequence}");

        if let Err(error) = attend(stream, &id, run, resize) {
          failed(error);
        }
      });
    }
  });
}

fn attend<R, S>(stream: UnixStream, id: &str, run: R, resize: S) -> io::Result<()>
where
  R: Fn(&Request, &Stdio, &str) -> i32,
  S: Fn(&str, RawFd) + Send,
{
  let (payload, received) = receive(&stream)?;

  // `served` connects and closes without sending; that is not an error.
  if probe(&payload, &received) {
    return Ok(());
  }

  // `received` keeps these open until the guest is done.
  let descriptors: Vec<RawFd> = received.iter().map(AsRawFd::as_raw_fd).collect();

  let (request, stdio) = Request::decode(&payload, &descriptors)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed request"))?;

  std::thread::scope(|scope| {
    let mut watching = stream.try_clone()?;
    let terminal = stdio.terminal;

    if stdio.has_terminal() {
      scope.spawn(move || {
        let mut byte = [0u8; 1];

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

/// `resized` returns whether this process's window changed size since last asked.
pub fn request(path: &Path, request: &Request, stdio: &Stdio, resized: &(dyn Fn() -> bool + Sync)) -> io::Result<i32> {
  let stream = UnixStream::connect(path)?;

  send(&stream, request.encode(stdio).as_bytes(), &descriptors(stdio))?;

  let wait = || {
    let mut code = [0u8; 4];
    (&stream).read_exact(&mut code)?;

    Ok(i32::from_be_bytes(code))
  };

  if !stdio.has_terminal() {
    return wait();
  }

  super::terminal::while_resizing(
    resized,
    || {
      let _ = (&stream).write_all(&[RESIZE]);
    },
    wait,
  )
}

fn send(stream: &UnixStream, payload: &[u8], descriptors: &[RawFd]) -> io::Result<()> {
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };
  let bytes = size_of_val(descriptors) as u32;

  // SAFETY: every pointer refers to a local that outlives the call, and the control buffer fits MAX_DESCRIPTORS.
  let sent = unsafe {
    let mut message: libc::msghdr = std::mem::zeroed();
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = space.as_mut_ptr() as *mut libc::c_void;
    // Not the buffer's capacity: any excess reads as a malformed second control message (EINVAL).
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

fn receive(stream: &UnixStream) -> io::Result<(String, Vec<OwnedFd>)> {
  let mut payload = vec![0u8; MAX_REQUEST];
  let mut space = [0u8; CMSG_SPACE];
  let mut iov = libc::iovec {
    iov_base: payload.as_mut_ptr() as *mut libc::c_void,
    iov_len: payload.len(),
  };

  // SAFETY: as in `send`; descriptors the kernel writes into the control buffer are ours to own and never exceed it.
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

  // A full buffer means the kernel truncated the request.
  if read == payload.len() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!("a request longer than {MAX_REQUEST} bytes"),
    ));
  }

  payload.truncate(read);

  Ok((String::from_utf8_lossy(&payload).into_owned(), descriptors))
}

const MAX_DESCRIPTORS: usize = 4;

/// An upper bound, since `libc::CMSG_SPACE` isn't const.
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
      working_directory: "/workspace/project".to_string(),
    }
  }

  fn received(stdio: &Stdio) -> Vec<RawFd> {
    (10..).take(descriptors(stdio).len()).collect()
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
  fn says_nothing_is_served_at_a_path_with_no_socket() {
    let directory = tempfile::tempdir().expect("a temp dir");

    assert!(!served(&directory.path().join(CONTROL_SOCKET)));
  }

  #[test]
  fn carries_three_descriptors_and_a_payload_across() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let path = directory.path().join(CONTROL_SOCKET);
    let listener = bind(&path).expect("bind");
    let stdio = Stdio::inherit(true);

    let sending = std::thread::spawn({
      let payload = request().encode(&stdio);
      let descriptors = descriptors(&stdio);

      move || {
        let client = UnixStream::connect(&path).expect("connect");
        send(&client, payload.as_bytes(), &descriptors).expect("send");
      }
    });

    let (stream, _) = listener.accept().expect("accept");
    let (payload, descriptors) = receive(&stream).expect("receive");

    sending.join().expect("the sender should finish");

    assert_eq!(descriptors.len(), 3, "stdin, stdout and stderr");

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
    let path = directory.path().join(CONTROL_SOCKET);

    drop(bind(&path).expect("the first bind"));

    assert!(path.exists(), "the inode outlives the listener");
    assert!(bind(&path).is_ok(), "a dead socket should not block the next owner");
  }
}
