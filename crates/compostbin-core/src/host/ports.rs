//! Relaying declared host ports to the guest, one vsock port per port (D11).
//!
//! The guest's own relay turns `localhost:<port>` into a vsock connection to
//! the host; this is the other half, accepting there and connecting to the
//! host's `127.0.0.1:<port>`. Vsock reaches only the VM that dialed it, so
//! nothing listens on a network address and no other container can reach a
//! forwarded port.

use crate::host::agent::POLL_INTERVAL;
use compostbin_engine::engine::{Engine, Listener};
use compostbin_engine::error::EngineError;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Loopback refuses at once; this only bounds a service that accepts nothing.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const BUFFER_SIZE: usize = 16 * 1024;

/// Added to a forwarded port to give its vsock port, well clear of the low
/// ports Containerization's own guest agent uses.
const VSOCK_PORT_BASE: u32 = 0x7000_0000;

/// One declared port: the vsock port the guest reaches it through, and the
/// host service behind it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forward {
  pub upstream: SocketAddr,
}

impl Forward {
  /// The same port on both sides, which is all the manifest can say.
  pub fn to_loopback(port: u16) -> Self {
    Self {
      upstream: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
    }
  }

  pub fn port(&self) -> u16 {
    self.upstream.port()
  }

  /// Derived from the port, so neither side has to be told the other's.
  pub fn vsock_port(&self) -> u32 {
    VSOCK_PORT_BASE + u32::from(self.port())
  }

  /// `<port>:<vsock port>`, as the guest's relay takes each one.
  pub fn guest_argument(&self) -> String {
    format!("{}:{}", self.port(), self.vsock_port())
  }
}

/// A forward the engine is listening for. It lives as long as the VM, which
/// dies with the process holding it.
pub struct Bound {
  forward: Forward,
  listener: Box<dyn Listener>,
}

/// What the relay has to say, left to the caller to print: core does not own
/// the terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortEvent {
  /// Listening, and so ready for the guest to connect.
  Listening(Forward),
  /// Nothing answers upstream. Once per run of failures, since a client
  /// retrying against a server that is not up yet would flood the terminal.
  UpstreamRefused(Forward, String),
}

/// In the terms a user would look for.
impl std::fmt::Display for PortEvent {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Listening(forward) => write!(
        formatter,
        "forwarding localhost:{} to {}",
        forward.port(),
        forward.upstream
      ),
      Self::UpstreamRefused(forward, error) => write!(formatter, "nothing answers at {}: {error}", forward.upstream),
    }
  }
}

/// Listens for every forward into the running container `name`, reporting each
/// as it comes up. All or nothing, so a session never forwards only some of
/// what it declared.
pub fn listen_all(
  engine: &impl Engine,
  name: &str,
  forwards: &[Forward],
  report: &(dyn Fn(PortEvent) + Sync),
) -> Result<Vec<Bound>, EngineError> {
  let mut bound = Vec::with_capacity(forwards.len());

  for forward in forwards {
    bound.push(Bound {
      forward: forward.clone(),
      listener: engine.listen(name, forward.vsock_port())?,
    });
    report(PortEvent::Listening(forward.clone()));
  }

  Ok(bound)
}

/// Relays every bound forward until `stop` is set, then returns once every
/// connection has closed.
pub fn relay(bound: &[Bound], stop: &AtomicBool, report: &(dyn Fn(PortEvent) + Sync)) {
  std::thread::scope(|scope| {
    for bound in bound {
      scope.spawn(move || accept(bound, stop, report));
    }

    while !stop.load(Ordering::Relaxed) {
      std::thread::sleep(POLL_INTERVAL);
    }

    // An `accept` watches no flag; finishing is what ends it.
    for bound in bound {
      bound.listener.finish();
    }
  });
}

/// Every connection to one forward, each on a thread of its own, until its
/// listener finishes.
fn accept(bound: &Bound, stop: &AtomicBool, report: &(dyn Fn(PortEvent) + Sync)) {
  let refused = AtomicBool::new(false);

  std::thread::scope(|scope| {
    while let Some(guest) = bound.listener.accept() {
      // A vsock socket, not a unix one, but this uses only what every stream
      // socket answers alike: reads, writes, shutdown and timeouts.
      let guest = UnixStream::from(guest);
      let refused = &refused;
      scope.spawn(move || connect(guest, &bound.forward, refused, stop, report));
    }
  });
}

/// One guest connection, relayed until both directions have ended.
fn connect(
  guest: UnixStream,
  forward: &Forward,
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
        report(PortEvent::UpstreamRefused(forward.clone(), error.to_string()));
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

/// The two ends of a relayed connection: a socket to the guest, a TCP stream to
/// the service.
///
/// `read_some` and `write_some` restate `Read` and `Write` for `&Self`. A
/// `for<'a> &'a Self: Read + Write` bound instead would not carry to `splice`
/// and `send` (a higher-ranked where-clause is not an implied bound), so every
/// caller would have to restate it.
trait Stream: Sync {
  fn read_some(&self, buffer: &mut [u8]) -> io::Result<usize>;
  fn write_some(&self, data: &[u8]) -> io::Result<usize>;
  fn shutdown_write(&self) -> io::Result<()>;

  /// Blocking, since an accepted socket may inherit its listener's
  /// non-blocking mode, but never for longer than a poll: every wait re-checks
  /// `stop`.
  fn configure(&self) -> io::Result<()>;
}

/// Both ends answer the same inherent methods, so both impls are the same
/// lines; a blanket one cannot reach `shutdown` or the timeout setters, which
/// no trait declares.
macro_rules! impl_stream {
  ($socket:ty) => {
    impl Stream for $socket {
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
  };
}

impl_stream!(UnixStream);
impl_stream!(TcpStream);

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
  use compostbin_engine::fake::{Call, RecordingEngine};
  use std::net::TcpListener;
  use std::sync::Mutex;
  use std::time::Instant;

  const DEADLINE: Duration = Duration::from_secs(5);
  const NAME: &str = "cb-ports";

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

  /// An upstream listener and the forward that reaches it.
  fn upstream_listener() -> (TcpListener, Forward) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
    let upstream = listener.local_addr().expect("upstream address");
    (listener, Forward { upstream })
  }

  /// Reads everything the guest sends, then echoes it back — so a reply at all
  /// means the guest's half-close reached upstream.
  fn serve_echo(listener: &TcpListener) {
    let mut stream = accept_within(listener);
    let mut received = Vec::new();
    stream
      .read_to_end(&mut received)
      .expect("read to the guest's EOF");
    stream.write_all(&received).expect("echo");
  }

  /// The same, `connections` times and blocking on accept: the test below times
  /// the relay's own accept, so its upstream must add no poll of its own.
  fn serve_echoes(listener: &TcpListener, connections: usize) {
    for _ in 0..connections {
      let Ok((mut stream, _)) = listener.accept() else { return };
      let mut received = Vec::new();

      if stream.read_to_end(&mut received).is_ok() {
        let _ = stream.write_all(&received);
      }
    }
  }

  fn round_trip(engine: &RecordingEngine, forward: &Forward, message: &str) -> String {
    let mut guest = engine
      .connect(forward.vsock_port())
      .expect("connect to the relay");
    guest.write_all(message.as_bytes()).expect("send");
    guest.shutdown(Shutdown::Write).expect("half-close");
    let mut reply = String::new();
    guest.read_to_string(&mut reply).expect("reply");
    reply
  }

  fn record(events: &Mutex<Vec<PortEvent>>) -> impl Fn(PortEvent) + Sync {
    |event| events.lock().expect("events").push(event)
  }

  fn listening(engine: &RecordingEngine, forward: &Forward, events: &Mutex<Vec<PortEvent>>) -> Vec<Bound> {
    listen_all(engine, NAME, std::slice::from_ref(forward), &record(events)).expect("listen")
  }

  #[test]
  fn derives_a_vsock_port_from_the_forwarded_one() {
    let forward = Forward::to_loopback(7001);

    assert_eq!(
      forward.upstream,
      "127.0.0.1:7001".parse::<SocketAddr>().expect("an address")
    );
    assert_eq!(forward.port(), 7001);
    assert_eq!(forward.vsock_port(), 0x7000_0000 + 7001);
    assert_eq!(forward.guest_argument(), format!("7001:{}", 0x7000_0000 + 7001));
  }

  #[test]
  fn listens_on_each_forwards_vsock_port() {
    let engine = RecordingEngine::new();
    let forwards = [Forward::to_loopback(7001), Forward::to_loopback(7002)];
    let events = Mutex::new(Vec::new());

    let bound = listen_all(&engine, NAME, &forwards, &record(&events)).expect("listen");

    assert_eq!(bound.len(), 2);
    assert_eq!(
      engine.calls(),
      forwards
        .each_ref()
        .map(|forward| Call::Listen(NAME.to_string(), forward.vsock_port()))
    );
    assert_eq!(events.into_inner().expect("events"), forwards.map(PortEvent::Listening));
  }

  #[test]
  fn relays_after_half_close() {
    let engine = RecordingEngine::new();
    let (upstream, forward) = upstream_listener();
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = listening(&engine, &forward, &events);

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&bound, &stop, &record(&events)));
      scope.spawn(|| serve_echo(&upstream));

      assert_eq!(round_trip(&engine, &forward, "hello"), "hello");

      stop.store(true, Ordering::Relaxed);
    });
  }

  /// Accepting must not wait out a `POLL_INTERVAL`. Timed because prompt and
  /// eventual differ only in duration; the budget is a quarter interval each.
  #[test]
  fn accepts_without_waiting_for_a_poll() {
    const CONNECTIONS: u32 = 10;

    let engine = RecordingEngine::new();
    let (upstream, forward) = upstream_listener();
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = listening(&engine, &forward, &events);

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&bound, &stop, &record(&events)));
      scope.spawn(|| serve_echoes(&upstream, CONNECTIONS as usize));

      let started = Instant::now();
      for _ in 0..CONNECTIONS {
        assert_eq!(round_trip(&engine, &forward, "hello"), "hello");
      }
      let elapsed = started.elapsed();

      stop.store(true, Ordering::Relaxed);

      assert!(
        elapsed < POLL_INTERVAL * CONNECTIONS / 4,
        "{CONNECTIONS} round trips took {elapsed:?}, so each waited for a poll"
      );
    });
  }

  #[test]
  fn refused_upstream_reports_once() {
    let engine = RecordingEngine::new();
    let forward = Forward { upstream: free_port() };
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = listening(&engine, &forward, &events);

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&bound, &stop, &record(&events)));

      for _ in 0..2 {
        assert_eq!(
          round_trip(&engine, &forward, "hello"),
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

  /// An idle connection must not keep the session's exit waiting, and the guest
  /// is refused from then on.
  #[test]
  fn stops_with_a_connection_open() {
    let engine = RecordingEngine::new();
    let (upstream, forward) = upstream_listener();
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());
    let bound = listening(&engine, &forward, &events);

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      let relaying = scope.spawn(|| relay(&bound, &stop, &record(&events)));

      let _guest = engine
        .connect(forward.vsock_port())
        .expect("connect to the relay");
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

    assert!(
      engine.connect(forward.vsock_port()).is_err(),
      "a stopped relay finishes its listeners"
    );
  }
}
