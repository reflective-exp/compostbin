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
          let _ = write!(stream, "the host says: {}", asked.trim());
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
  project.compostbin(&[
    "exec",
    "bash",
    "-c",
    &format!("exec 3<>/dev/tcp/127.0.0.1/{port} && printf '{question}\\n' >&3 && head -c 64 <&3",),
  ])
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
