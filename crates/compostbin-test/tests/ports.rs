#![cfg(feature = "integration")]
//! `[host] ports`: a host service on a fixed port answering at the guest's own
//! `localhost`.
//!
//! Each port is relayed through a unix socket carried into one container, so
//! nothing listens on a network address and no other container can reach it.
//! Both halves of that are worth a session: that the relay carries a request
//! and its answer, and that a port nobody declared is not reachable.

use compostbin_test::{Project, stderr};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::fs::FileTypeExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// A host service on loopback that answers each line with what it was given.
/// Bound before the manifest is written, since the manifest has to name the
/// port it landed on.
struct Service {
  port: u16,
  stop: Arc<AtomicBool>,
  thread: Option<JoinHandle<()>>,
}

impl Service {
  fn start() -> Self {
    Self::answering("the host")
  }

  /// The same, naming itself in its answers: two services on two ports are
  /// otherwise indistinguishable.
  fn answering(who: &'static str) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a host port");
    let port = listener.local_addr().expect("the bound address").port();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);

    let thread = std::thread::spawn(move || {
      for connection in listener.incoming() {
        if stopping.load(Ordering::Relaxed) {
          return;
        }

        let Ok(mut stream) = connection else { continue };
        let mut asked = String::new();
        if BufReader::new(stream.try_clone().expect("clone"))
          .read_line(&mut asked)
          .is_ok()
        {
          let _ = write!(stream, "{who} says: {}", asked.trim());
        }
        let _ = stream.shutdown(Shutdown::Both);
      }
    });

    Self {
      port,
      stop,
      thread: Some(thread),
    }
  }
}

/// A port number nothing is listening on: bound to learn a free one, then
/// released.
fn unused_port() -> u16 {
  TcpListener::bind("127.0.0.1:0")
    .expect("bind a host port")
    .local_addr()
    .expect("the bound address")
    .port()
}

impl Drop for Service {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    // Unblocks `incoming`, which is waiting rather than watching the flag.
    let _ = TcpStream::connect(("127.0.0.1", self.port));
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

/// What the guest sends and reads back, over its own loopback. `bash` because
/// `/dev/tcp` is a bash feature and the guest's `sh` is not bash.
fn ask(project: &Project, port: u16, question: &str) -> std::process::Output {
  project.compostbin(&["exec", "bash", "-c", &exchange(port, question)])
}

/// One question and its answer, as a shell line, so a test can put more than
/// one of them in a single container.
fn exchange(port: u16, question: &str) -> String {
  format!("exec 3<>/dev/tcp/127.0.0.1/{port} && printf '{question}\\n' >&3 && head -c 64 <&3")
}

#[test]
fn declared_port_reaches_the_host() {
  let service = Service::start();
  let project = Project::new("cbt-ports");
  project.manifest(&format!(
    r#"
[host]
ports = [{}]
"#,
    service.port
  ));

  let output = ask(&project, service.port, "hello");

  assert!(
    output.status.success(),
    "the guest could not reach the port: {}",
    stderr(&output)
  );
  assert_eq!(
    String::from_utf8_lossy(&output.stdout),
    "the host says: hello",
    "a host service answers at the guest's own localhost"
  );
}

/// The socket is the relay, and it lives in the session directory: nothing is
/// bound on a network address, so no other container can reach it.
#[test]
fn port_is_relayed_through_a_socket() {
  let service = Service::start();
  // Short, because the socket beneath it must fit the 104 bytes a unix socket
  // path may have.
  let project = Project::new("cbt-ports-sock");
  project.manifest(&format!(
    r#"
[host]
ports = [{}]
"#,
    service.port
  ));

  // The sockets belong to whichever process created the container, and are
  // bound for exactly as long as it runs — so one has to be running to look.
  // Dropping the guard kills it, and the container with it.
  let running = project.guest_in_background("sleep 60");
  let socket = project
    .state_dir()
    .join(format!("ports/{}.sock", service.port));

  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
  while !socket.exists() && std::time::Instant::now() < deadline {
    std::thread::sleep(std::time::Duration::from_millis(50));
  }

  let kind = std::fs::symlink_metadata(&socket)
    .unwrap_or_else(|error| panic!("nothing was bound at {}: {error}", socket.display()))
    .file_type();

  assert!(
    kind.is_socket(),
    "{} is not a socket, so it would be carried into the guest as a filesystem",
    socket.display()
  );

  drop(running);
}

/// Each declared port is a socket and a relay of its own: two services do not
/// share one, and neither answers for the other.
#[test]
fn each_declared_port_reaches_its_own_service() {
  let first = Service::answering("the first");
  let second = Service::answering("the second");
  let project = Project::new("cbt-ports-two");
  project.manifest(&format!(
    r#"
[host]
ports = [{}, {}]
"#,
    first.port, second.port
  ));

  // Both in one container: each `exec` would otherwise be a session of its own.
  let output = project.compostbin(&[
    "exec",
    "bash",
    "-c",
    &format!(
      "{}; echo; {}",
      exchange(first.port, "hello"),
      exchange(second.port, "hello")
    ),
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    String::from_utf8_lossy(&output.stdout),
    "the first says: hello\nthe second says: hello",
    "each port carried its own service's answer"
  );
}

/// A declared port with nothing behind it is ordinary — `[host.commands]` may
/// well be what starts the service — so the relay records it and the session
/// carries on.
#[test]
fn a_port_with_nothing_behind_it_is_recorded_not_fatal() {
  let port = unused_port();
  let project = Project::new("cbt-ports-dead");
  project.manifest(&format!(
    r#"
[host]
ports = [{port}]
"#
  ));

  let unanswered = ask(&project, port, "anyone there");

  assert_eq!(
    String::from_utf8_lossy(&unanswered.stdout),
    "",
    "the relay has nothing to answer with"
  );
  assert_eq!(
    project.guest_output("echo the session is still up"),
    "the session is still up",
    "and a service that is not running does not take the session with it"
  );

  let log = std::fs::read_to_string(project.state_dir().join("ports.log")).unwrap_or_default();

  assert!(
    log.contains(&format!("nothing answers at 127.0.0.1:{port}")),
    "the relay writes what it could not reach where the terminal is Claude's: {log}"
  );
}

#[test]
fn undeclared_port_is_unreachable() {
  let service = Service::start();
  let project = Project::new("cbt-ports-none");

  let output = ask(&project, service.port, "hello");

  assert!(
    !output.status.success(),
    "a host port reaches the guest only because the manifest declared it"
  );
}
