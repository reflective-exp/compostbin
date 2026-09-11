//! Relaying declared host ports to the guest over vmnet (D10).
//!
//! The guest's own relay turns `localhost:<port>` into `<gateway>:<port>`; this
//! is the other half, turning `<gateway>:<port>` into the host's `127.0.0.1`.
//! The gateway is on the shared vmnet bridge, so every container can reach what
//! is listening here — the cost D10 accepts.

use crate::host::POLL_INTERVAL;
use std::io::{self, ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Loopback refuses at once; this only bounds a service that accepts nothing.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
const BUFFER_SIZE: usize = 16 * 1024;

/// One declared port: where the guest connects, and where the service is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forward {
  pub listen: SocketAddr,
  pub upstream: SocketAddr,
}

impl Forward {
  /// The same port on both sides, which is all the manifest can say.
  pub fn to_loopback(gateway: IpAddr, port: u16) -> Self {
    Self {
      listen: SocketAddr::new(gateway, port),
      upstream: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
    }
  }
}

/// What the relay has to say, left to the caller to print: core does not own
/// the terminal.
#[derive(Clone, Debug, PartialEq)]
pub enum PortEvent {
  /// Bound, whether at once or by taking over from another session.
  Listening(Forward),
  /// Another session holds the address and, the bridge being shared, already
  /// serves this one too. Retried every poll, so its exit hands the port over.
  Deferred(Forward),
  /// Any other bind failure. Reported once, and retried like a deferral.
  Unbindable(Forward, String),
  /// Nothing answers upstream. Once per run of failures, since a client
  /// retrying against a server that is not up yet would flood the terminal.
  UpstreamRefused(Forward, String),
}

/// Relays every forward until `stop` is set, then returns once every
/// connection has closed.
pub fn relay(forwards: &[Forward], stop: &AtomicBool, report: &(dyn Fn(PortEvent) + Sync)) {
  // Outside the scope, so connection threads can borrow them while `slots`
  // stays the polling loop's alone.
  let refused: Vec<AtomicBool> = forwards.iter().map(|_| AtomicBool::new(false)).collect();
  let mut slots: Vec<Slot> = forwards.iter().copied().map(Slot::new).collect();

  std::thread::scope(|scope| {
    while !stop.load(Ordering::Relaxed) {
      for (slot, refused) in slots.iter_mut().zip(&refused) {
        if slot.listener.is_none() {
          slot.bind(report);
        }

        let Some(listener) = &slot.listener else {
          continue;
        };

        // Every connection waiting, not one per poll.
        while let Ok((guest, _)) = listener.accept() {
          let forward = slot.forward;
          scope.spawn(move || connect(guest, forward, refused, stop, report));
        }
      }

      std::thread::sleep(POLL_INTERVAL);
    }
  });
}

/// A forward's listener once it has one, and what has already been said about
/// it, so a retry every poll is not a report every poll.
struct Slot {
  forward: Forward,
  listener: Option<TcpListener>,
  deferred: bool,
  unbindable: bool,
}

impl Slot {
  fn new(forward: Forward) -> Self {
    Self {
      forward,
      listener: None,
      deferred: false,
      unbindable: false,
    }
  }

  /// `std` sets `SO_REUSEADDR`, so only an exact-address collision fails here —
  /// a wildcard service on the same port is shadowed rather than refused (F18).
  fn bind(&mut self, report: &(dyn Fn(PortEvent) + Sync)) {
    let bound = TcpListener::bind(self.forward.listen).and_then(|listener| {
      listener.set_nonblocking(true)?;
      Ok(listener)
    });

    match bound {
      Ok(listener) => {
        self.listener = Some(listener);
        report(PortEvent::Listening(self.forward));
      }
      Err(error) if error.kind() == ErrorKind::AddrInUse => {
        if !self.deferred {
          self.deferred = true;
          report(PortEvent::Deferred(self.forward));
        }
      }
      Err(error) => {
        if !self.unbindable {
          self.unbindable = true;
          report(PortEvent::Unbindable(self.forward, error.to_string()));
        }
      }
    }
  }
}

/// One guest connection, relayed until both directions have ended.
fn connect(
  guest: TcpStream,
  forward: Forward,
  refused: &AtomicBool,
  stop: &AtomicBool,
  report: &(dyn Fn(PortEvent) + Sync),
) {
  if configure(&guest).is_err() {
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

  if configure(&upstream).is_err() {
    return;
  }

  std::thread::scope(|scope| {
    scope.spawn(|| splice(&guest, &upstream, stop));
    splice(&upstream, &guest, stop);
  });
}

/// Blocking, since accepted sockets inherit the listener's non-blocking mode on
/// macOS, but never for longer than a poll: every wait re-checks `stop`.
fn configure(stream: &TcpStream) -> io::Result<()> {
  stream.set_nonblocking(false)?;
  stream.set_read_timeout(Some(POLL_INTERVAL))?;
  stream.set_write_timeout(Some(POLL_INTERVAL))
}

/// An EOF for the guest, then whatever it already sent is read and dropped:
/// closing with unread data would reset the connection instead.
fn close_cleanly(guest: &TcpStream) {
  let _ = guest.shutdown(Shutdown::Write);

  let mut reader = guest;
  let mut discard = [0; BUFFER_SIZE];
  while let Ok(read) = reader.read(&mut discard) {
    if read == 0 {
      break;
    }
  }
}

/// One direction, until EOF, an error, or `stop` — then the EOF is passed on,
/// so a half-close reaches the other side.
fn splice(from: &TcpStream, to: &TcpStream, stop: &AtomicBool) {
  let mut reader = from;
  let mut buffer = [0; BUFFER_SIZE];

  while !stop.load(Ordering::Relaxed) {
    match reader.read(&mut buffer) {
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

  let _ = to.shutdown(Shutdown::Write);
}

/// `write_all`, except that a stalled peer is waited on a poll at a time, so
/// `stop` still ends it.
fn send(to: &TcpStream, mut data: &[u8], stop: &AtomicBool) -> bool {
  let mut writer = to;

  while !data.is_empty() {
    match writer.write(data) {
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
  use std::io::{Read, Write};
  use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
  use std::sync::Mutex;
  use std::sync::atomic::Ordering;
  use std::time::{Duration, Instant};

  const DEADLINE: Duration = Duration::from_secs(5);

  /// A port nothing is listening on, found by binding and letting go.
  fn free_port() -> SocketAddr {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
      .and_then(|listener| listener.local_addr())
      .expect("an ephemeral port")
  }

  fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let started = Instant::now();
    while !condition() {
      assert!(started.elapsed() < DEADLINE, "timed out waiting for {what}");
      std::thread::sleep(Duration::from_millis(10));
    }
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
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
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

  fn round_trip(listen: SocketAddr, message: &str) -> String {
    let mut guest = TcpStream::connect(listen).expect("connect to the relay");
    guest.write_all(message.as_bytes()).expect("send");
    guest.shutdown(Shutdown::Write).expect("half-close");
    let mut reply = String::new();
    guest.read_to_string(&mut reply).expect("reply");
    reply
  }

  fn listening(events: &Mutex<Vec<PortEvent>>, forward: Forward) -> bool {
    events
      .lock()
      .expect("events")
      .contains(&PortEvent::Listening(forward))
  }

  #[test]
  fn forwards_to_loopback() {
    let gateway: IpAddr = "192.168.64.1".parse().expect("an address");

    assert_eq!(
      Forward::to_loopback(gateway, 7001),
      Forward {
        listen: "192.168.64.1:7001".parse().expect("an address"),
        upstream: "127.0.0.1:7001".parse().expect("an address"),
      }
    );
  }

  #[test]
  fn relays_after_half_close() {
    let (upstream, address) = echo_once();
    let forward = Forward {
      listen: free_port(),
      upstream: address,
    };
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&[forward], &stop, &|event| events.lock().expect("events").push(event)));
      scope.spawn(|| serve_echo(&upstream));

      wait_for("the relay to bind", || listening(&events, forward));
      assert_eq!(round_trip(forward.listen, "hello"), "hello");

      stop.store(true, Ordering::Relaxed);
    });
  }

  #[test]
  fn refused_upstream_reports_once() {
    let forward = Forward {
      listen: free_port(),
      upstream: free_port(),
    };
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&[forward], &stop, &|event| events.lock().expect("events").push(event)));
      wait_for("the relay to bind", || listening(&events, forward));

      for _ in 0..2 {
        assert_eq!(
          round_trip(forward.listen, "hello"),
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

  /// Another session's listener on the same address: this one waits, and
  /// takes the port once that one lets go.
  #[test]
  fn defers_then_takes_over() {
    let (upstream, address) = echo_once();
    let listen = free_port();
    let holder = TcpListener::bind(listen).expect("the other session's listener");
    let forward = Forward {
      listen,
      upstream: address,
    };
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      scope.spawn(|| relay(&[forward], &stop, &|event| events.lock().expect("events").push(event)));
      scope.spawn(|| serve_echo(&upstream));

      wait_for("the relay to defer", || {
        events
          .lock()
          .expect("events")
          .contains(&PortEvent::Deferred(forward))
      });
      drop(holder);

      wait_for("the relay to take over", || listening(&events, forward));
      assert_eq!(round_trip(listen, "hello"), "hello");

      stop.store(true, Ordering::Relaxed);
    });

    let deferrals = events
      .lock()
      .expect("events")
      .iter()
      .filter(|event| matches!(event, PortEvent::Deferred(..)))
      .count();
    assert_eq!(deferrals, 1, "a deferral is reported once, not every poll");
  }

  /// An idle connection must not keep the session's exit waiting.
  #[test]
  fn stops_with_a_connection_open() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("upstream");
    let forward = Forward {
      listen: free_port(),
      upstream: upstream.local_addr().expect("upstream address"),
    };
    let stop = AtomicBool::new(false);
    let events = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
      let _stop = StopOnDrop(&stop);
      let relaying = scope.spawn(|| relay(&[forward], &stop, &|event| events.lock().expect("events").push(event)));
      wait_for("the relay to bind", || listening(&events, forward));

      let _guest = TcpStream::connect(forward.listen).expect("connect to the relay");
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
}
