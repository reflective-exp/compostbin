//! Relaying declared host ports to the guest, one unix socket per port (D11).
//!
//! The guest's own relay turns `localhost:<port>` into a connection to
//! `/run/compostbin/ports/<port>.sock`; this is the other half, accepting there
//! and connecting to the host's `127.0.0.1:<port>`. `container` relays the
//! socket into the guest itself — a socket passed as `--volume` becomes a vsock
//! relay rather than a mount — so nothing listens on a network address and no
//! other container can reach a forwarded port.

use crate::host::POLL_INTERVAL;
use apple_container::engine::Engine;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Loopback refuses at once; this only bounds a service that accepts nothing.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const BUFFER_SIZE: usize = 16 * 1024;
/// Asking the daemon what is running costs a subprocess, so this is far slower
/// than the relay's own poll. Nothing waits on it: it only decides when a relay
/// nobody needs any more gives up.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// How long the relay waits for the container to be created after it binds.
pub const APPEAR_GRACE: Duration = Duration::from_secs(120);
/// How long a container may be absent before the relay takes it as gone for
/// good rather than restarting.
pub const VANISH_GRACE: Duration = Duration::from_secs(30);

/// The guest end of the relay is `root:root` with this socket's mode copied
/// verbatim, and the session runs as `claude`: anything short of
/// world-accessible is refused inside the container. The session directory is
/// what confines these, not the socket mode.
pub const SOCKET_MODE: u32 = 0o666;
pub const SOCKET_SUFFIX: &str = ".sock";

/// One declared port: the socket the guest reaches through, and the host
/// service behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forward {
  pub listen: PathBuf,
  pub upstream: SocketAddr,
}

impl Forward {
  /// The same port on both sides, which is all the manifest can say. The socket
  /// is named after the port so the guest can find it without being told.
  pub fn to_loopback(directory: &Path, port: u16) -> Self {
    Self {
      listen: directory.join(format!("{port}{SOCKET_SUFFIX}")),
      upstream: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
    }
  }

  pub fn port(&self) -> u16 {
    self.upstream.port()
  }
}

/// A bound listener. It must outlive the container created with it: the relay
/// is attached to the inode that existed at creation, so a socket unlinked and
/// rebound at the same path leaves the guest with a dead socket that only
/// recreating the container heals.
#[derive(Debug)]
pub struct Bound {
  forward: Forward,
  listener: UnixListener,
}

impl Bound {
  pub fn forward(&self) -> &Forward {
    &self.forward
  }
}

/// What the relay has to say, left to the caller to print: core does not own
/// the terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortEvent {
  /// Bound, and so ready to be mounted into a container.
  Listening(Forward),
  /// Nothing answers upstream. Once per run of failures, since a client
  /// retrying against a server that is not up yet would flood the terminal.
  UpstreamRefused(Forward, String),
}

/// Whether something is already accepting on this socket — another agent
/// serving the same session. A refused connection means a socket left behind by
/// one that died, and a missing one means nothing has bound it yet; neither is
/// anybody's.
pub fn served(forward: &Forward) -> bool {
  UnixStream::connect(&forward.listen).is_ok()
}

/// Binds every forward, reporting each as it comes up. All or nothing: a
/// container started with only some of its sockets would forward only some of
/// its ports, and no restart short of recreating it would fix that.
pub fn bind_all(forwards: &[Forward], report: &(dyn Fn(PortEvent) + Sync)) -> io::Result<Vec<Bound>> {
  let mut bound = Vec::with_capacity(forwards.len());

  for forward in forwards {
    bound.push(bind(forward)?);
    report(PortEvent::Listening(forward.clone()));
  }

  Ok(bound)
}

/// Binds one forward, replacing whatever sits at the path first — a socket left
/// by a session that died, or a symlink where the mount source belongs, which
/// would take the container down at start rather than degrade.
fn bind(forward: &Forward) -> io::Result<Bound> {
  if let Some(parent) = forward.listen.parent() {
    fs::create_dir_all(parent)?;
  }

  match fs::remove_file(&forward.listen) {
    Ok(()) => {}
    Err(error) if error.kind() == ErrorKind::NotFound => {}
    Err(error) => return Err(error),
  }

  let listener = UnixListener::bind(&forward.listen)?;
  listener.set_nonblocking(true)?;
  fs::set_permissions(&forward.listen, fs::Permissions::from_mode(SOCKET_MODE))?;

  Ok(Bound {
    forward: forward.clone(),
    listener,
  })
}

/// Sets `stop` when the container these sockets serve is gone for good, so the
/// relay outlives the `run` that started it and nothing else.
///
/// Two graces, because neither edge is instant: the container does not exist
/// yet when the relay binds — it cannot, the sockets have to be there first —
/// and `add --restart` takes it away and puts it back, which must not be read
/// as the session ending.
pub fn watch(container: &str, engine: &impl Engine, stop: &AtomicBool, appear: Duration, vanish: Duration) {
  let mut appeared = false;
  let mut waiting_since = Instant::now();

  while !stop.load(Ordering::Relaxed) {
    let running = engine
      .containers()
      .map(|containers| containers.iter().any(|name| name == container))
      .unwrap_or(true);

    if running {
      appeared = true;
      waiting_since = Instant::now();
    } else {
      let grace = if appeared { vanish } else { appear };

      if waiting_since.elapsed() > grace {
        stop.store(true, Ordering::Relaxed);
        return;
      }
    }

    std::thread::sleep(WATCH_INTERVAL);
  }
}

/// Relays every bound socket until `stop` is set, then returns once every
/// connection has closed.
pub fn relay(bound: &[Bound], stop: &AtomicBool, report: &(dyn Fn(PortEvent) + Sync)) {
  // Outside the scope, so connection threads can borrow them.
  let refused: Vec<AtomicBool> = bound.iter().map(|_| AtomicBool::new(false)).collect();

  std::thread::scope(|scope| {
    while !stop.load(Ordering::Relaxed) {
      for (bound, refused) in bound.iter().zip(&refused) {
        // Every connection waiting, not one per poll.
        while let Ok((guest, _)) = bound.listener.accept() {
          let forward = bound.forward.clone();
          scope.spawn(move || connect(guest, forward, refused, stop, report));
        }
      }

      std::thread::sleep(POLL_INTERVAL);
    }
  });
}

/// One guest connection, relayed until both directions have ended.
fn connect(
  guest: UnixStream,
  forward: Forward,
  refused: &AtomicBool,
  stop: &AtomicBool,
  report: &(dyn Fn(PortEvent) + Sync),
) {
  if guest.configure().is_err() {
    return;
  }

  let upstream = match TcpStream::connect_timeout(&forward.upstream, CONNECT_TIMEOUT) {
    Ok(upstream) => {
      refused.store(false, Ordering::Relaxed);
      upstream
    }
    Err(error) => {
      if !refused.swap(true, Ordering::Relaxed) {
        report(PortEvent::UpstreamRefused(forward, error.to_string()));
      }
      close_cleanly(&guest);
      return;
    }
  };

  if upstream.configure().is_err() {
    return;
  }

  std::thread::scope(|scope| {
    scope.spawn(|| splice(&guest, &upstream, stop));
    splice(&upstream, &guest, stop);
  });
}

/// The two ends of a relayed connection: a unix socket to the guest, a TCP
/// stream to the service. Only four operations differ between them, and the
/// copying below is the same either way.
/// The two ends of a relayed connection: a unix socket to the guest, a TCP
/// stream to the service. Only four operations differ between them, and the
/// copying below is the same either way.
///
/// `read_some` and `write_some` restate `Read` and `Write` for `&Self`, which
/// both types already implement. Hoisting them into a `for<'a> &'a Self: Read +
/// Write` bound on the trait does not carry to `splice` and `send`: a
/// higher-ranked where-clause is not an implied bound, so all three callers
/// would have to name the type parameter and restate it. That costs more than
/// the delegation it saves.
trait Stream: Sync {
  fn read_some(&self, buffer: &mut [u8]) -> io::Result<usize>;
  fn write_some(&self, data: &[u8]) -> io::Result<usize>;
  fn shutdown_write(&self) -> io::Result<()>;

  /// Blocking, since accepted sockets inherit the listener's non-blocking mode
  /// on macOS, but never for longer than a poll: every wait re-checks `stop`.
  fn configure(&self) -> io::Result<()>;
}

impl Stream for UnixStream {
  fn read_some(&self, buffer: &mut [u8]) -> io::Result<usize> {
    (&mut &*self).read(buffer)
  }

  fn write_some(&self, data: &[u8]) -> io::Result<usize> {
    (&mut &*self).write(data)
  }

  fn shutdown_write(&self) -> io::Result<()> {
    self.shutdown(Shutdown::Write)
  }

  fn configure(&self) -> io::Result<()> {
    self.set_nonblocking(false)?;
    self.set_read_timeout(Some(POLL_INTERVAL))?;
    self.set_write_timeout(Some(POLL_INTERVAL))
  }
}

impl Stream for TcpStream {
  fn read_some(&self, buffer: &mut [u8]) -> io::Result<usize> {
    (&mut &*self).read(buffer)
  }

  fn write_some(&self, data: &[u8]) -> io::Result<usize> {
    (&mut &*self).write(data)
  }

  fn shutdown_write(&self) -> io::Result<()> {
    self.shutdown(Shutdown::Write)
  }

  fn configure(&self) -> io::Result<()> {
    self.set_nonblocking(false)?;
    self.set_read_timeout(Some(POLL_INTERVAL))?;
    self.set_write_timeout(Some(POLL_INTERVAL))
  }
}

/// An EOF for the guest, then whatever it already sent is read and dropped:
/// closing with unread data would reset the connection instead.
fn close_cleanly(guest: &impl Stream) {
  let _ = guest.shutdown_write();

  let mut discard = [0; BUFFER_SIZE];
  while let Ok(read) = guest.read_some(&mut discard) {
    if read == 0 {
      break;
    }
  }
}

/// One direction, until EOF, an error, or `stop` — then the EOF is passed on,
/// so a half-close reaches the other side.
fn splice(from: &impl Stream, to: &impl Stream, stop: &AtomicBool) {
  let mut buffer = [0; BUFFER_SIZE];

  while !stop.load(Ordering::Relaxed) {
    match from.read_some(&mut buffer) {
      Ok(0) => break,
      Ok(read) => {
        if !send(to, &buffer[..read], stop) {
          break;
        }
      }
      Err(error) if waiting(&error) => {}
      Err(_) => break,
    }
  }

  let _ = to.shutdown_write();
}

/// `write_all`, except that a stalled peer is waited on a poll at a time, so
/// `stop` still ends it.
fn send(to: &impl Stream, mut data: &[u8], stop: &AtomicBool) -> bool {
  while !data.is_empty() {
    match to.write_some(data) {
      Ok(0) => return false,
      Ok(written) => data = &data[written..],
      Err(error) if waiting(&error) => {
        if stop.load(Ordering::Relaxed) {
          return false;
        }
      }
      Err(_) => return false,
    }
  }

  true
}

/// A timeout is a poll, not a failure.
fn waiting(error: &io::Error) -> bool {
  matches!(
    error.kind(),
    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::TcpListener;
  use std::sync::Mutex;
  use std::time::Instant;
  use tempfile::TempDir;

  const DEADLINE: Duration = Duration::from_secs(5);

  /// A port nothing is listening on, found by binding and letting go.
  fn free_port() -> SocketAddr {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
      .and_then(|listener| listener.local_addr())
      .expect("an ephemeral port")
  }

  /// A failing test must fail, not hang: a blocking `accept` would wait forever
  /// for a relay that is broken.
  fn accept_within(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).expect("non-blocking");
    let started = Instant::now();
    loop {
      match listener.accept() {
        Ok((stream, _)) => {
          // Accepted sockets inherit non-blocking on macOS.
          stream.set_nonblocking(false).expect("blocking");
          return stream;
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => {
          assert!(
            started.elapsed() < DEADLINE,
            "timed out waiting for a relayed connection"
          );
          std::thread::sleep(Duration::from_millis(10));
        }
        Err(error) => panic!("accept: {error}"),
      }
    }
  }

  /// Sets `stop` however the scope exits, so a failed assertion still lets the
  /// relay return and the scope join.
  struct StopOnDrop<'a>(&'a AtomicBool);

  impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
      self.0.store(true, Ordering::Relaxed);
    }
  }

  /// Reads everything the guest sends, then echoes it back — so a reply at all
  /// means the guest's half-close reached upstream.
  fn echo_once() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
    let address = listener.local_addr().expect("upstream address");
    (listener, address)
  }

  fn serve_echo(listener: &TcpListener) {
    let mut stream = accept_within(listener);
    let mut received = Vec::new();
    stream
      .read_to_end(&mut received)
      .expect("read to the guest's EOF");
    stream.write_all(&received).expect("echo");
  }

  fn round_trip(socket: &Path, message: &str) -> String {
    let mut guest = UnixStream::connect(socket).expect("connect to the relay");
    guest.write_all(message.as_bytes()).expect("send");
    guest.shutdown(Shutdown::Write).expect("half-close");
    let mut reply = String::new();
    guest.read_to_string(&mut reply).expect("reply");
    reply
  }

  /// A forward whose socket lives in `directory` and whose upstream is a real
  /// address, so only the guest side is under test.
  fn forward(directory: &TempDir, upstream: SocketAddr) -> Forward {
    Forward {
      listen: directory.path().join(format!("{}.sock", upstream.port())),
      upstream,
    }
  }

  fn record(events: &Mutex<Vec<PortEvent>>) -> impl Fn(PortEvent) + Sync {
    |event| events.lock().expect("events").push(event)
  }

  #[test]
  fn names_a_socket_after_its_port() {
    let forward = Forward::to_loopback(Path::new("/state/ports"), 7001);

    assert_eq!(forward.listen, PathBuf::from("/state/ports/7001.sock"));
    assert_eq!(
      forward.upstream,
      "127.0.0.1:7001".parse::<SocketAddr>().expect("an address")
    );
    assert_eq!(forward.port(), 7001);
  }

  /// The guest end is root-owned with this mode copied, and the session is not
  /// root: anything narrower is unreachable from inside the container.
  #[test]
  fn binds_world_accessible() {
    let directory = TempDir::new().expect("temp dir");
    let forward = forward(&directory, free_port());
    let events = Mutex::new(Vec::new());

    let bound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");

    let mode = fs::metadata(&forward.listen)
      .expect("the socket")
      .permissions()
      .mode();
    assert_eq!(mode & 0o777, SOCKET_MODE, "{mode:o}");
    assert_eq!(bound.len(), 1);
    assert_eq!(events.into_inner().expect("events"), [PortEvent::Listening(forward)]);
  }

  /// A session that died leaves its socket behind; the next one owns the path.
  #[test]
  fn binding_replaces_a_stale_socket() {
    let directory = TempDir::new().expect("temp dir");
    let forward = forward(&directory, free_port());
    let events = Mutex::new(Vec::new());

    let stale = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");
    drop(stale);
    assert!(forward.listen.exists(), "the path outlives the listener");
    assert!(!served(&forward), "nothing is accepting on it");

    // Bound, not dropped: the listener is what makes the socket answer.
    let _rebound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("rebind");
    assert!(served(&forward));
  }

  /// The socket another agent is still serving: this is what keeps a second
  /// `run` from unbinding the session's live relay.
  #[test]
  fn served_is_true_only_while_something_accepts() {
    let directory = TempDir::new().expect("temp dir");
    let forward = forward(&directory, free_port());
    let events = Mutex::new(Vec::new());

    assert!(!served(&forward), "nothing is bound yet");

    let bound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");
    assert!(served(&forward));

    drop(bound);
    assert!(!served(&forward));
  }

  #[test]
  fn relays_after_half_close() {
    let directory = TempDir::new().expect("temp dir");
    let (upstream, address) = echo_once();
    let forward = forward(&directory, address);
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&bound, &stop, &record(&events)));
      scope.spawn(|| serve_echo(&upstream));

      assert_eq!(round_trip(&forward.listen, "hello"), "hello");

      stop.store(true, Ordering::Relaxed);
    });
  }

  #[test]
  fn refused_upstream_reports_once() {
    let directory = TempDir::new().expect("temp dir");
    let forward = forward(&directory, free_port());
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&bound, &stop, &record(&events)));

      for _ in 0..2 {
        assert_eq!(
          round_trip(&forward.listen, "hello"),
          "",
          "a refused upstream closes the guest"
        );
      }

      stop.store(true, Ordering::Relaxed);
    });

    let refusals = events
      .lock()
      .expect("events")
      .iter()
      .filter(|event| matches!(event, PortEvent::UpstreamRefused(..)))
      .count();
    assert_eq!(refusals, 1);
  }

  /// An idle connection must not keep the session's exit waiting.
  #[test]
  fn stops_with_a_connection_open() {
    let directory = TempDir::new().expect("temp dir");
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
    let forward = forward(&directory, upstream.local_addr().expect("upstream address"));
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = bind_all(std::slice::from_ref(&forward), &record(&events)).expect("bind");

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      let relaying = scope.spawn(|| relay(&bound, &stop, &record(&events)));

      let _guest = UnixStream::connect(&forward.listen).expect("connect to the relay");
      let _held = accept_within(&upstream);

      stop.store(true, Ordering::Relaxed);
      let stopped = Instant::now();
      relaying.join().expect("the relay should not panic");
      assert!(
        stopped.elapsed() < Duration::from_secs(1),
        "took {:?}",
        stopped.elapsed()
      );
    });
  }

  /// Every socket or none: a container created with half its ports bound would
  /// forward half of them until it was recreated.
  #[test]
  fn binding_fails_whole() {
    let directory = TempDir::new().expect("temp dir");
    let good = forward(&directory, free_port());
    let unbindable = Forward {
      listen: directory
        .path()
        .join("missing")
        .join("nested")
        .join("7002.sock"),
      upstream: free_port(),
    };
    fs::write(directory.path().join("missing"), "not a directory").expect("a file in the way");
    let events = Mutex::new(Vec::new());

    let error = bind_all(&[good.clone(), unbindable], &record(&events)).expect_err("should fail");

    assert!(!matches!(error.kind(), ErrorKind::NotFound), "{error}");
    assert_eq!(
      events.into_inner().expect("events"),
      [PortEvent::Listening(good)],
      "the forwards before the failure were reported, and the caller gives up"
    );
  }
}
