#![cfg(feature = "integration")]

use compostbin_test::{Project, stderr, stdout, unused_port};
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

struct Service {
  port: u16,
  stop: Arc<AtomicBool>,
  thread: Option<JoinHandle<()>>,
}

impl Service {
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

/// `bash` because `/dev/tcp` is a bash feature and the guest's `sh` is not bash.
fn ask(project: &Project, port: u16, question: &str) -> std::process::Output {
  project.compostbin(&["exec", "bash", "-c", &exchange(port, question)])
}

fn exchange(port: u16, question: &str) -> String {
  format!("exec 3<>/dev/tcp/127.0.0.1/{port} && printf '{question}\\n' >&3 && head -c 64 <&3")
}

#[test]
fn declared_port_reaches_the_host() {
  let service = Service::answering("the host");
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
    stdout(&output),
    "the host says: hello",
    "a host service answers at the guest's own localhost"
  );
}

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

  // One `exec`, because each `exec` creates its own container.
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
    stdout(&output),
    "the first says: hello\nthe second says: hello",
    "each port carried its own service's answer"
  );
}

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

  assert_eq!(stdout(&unanswered), "", "the relay has nothing to answer with");
  assert_eq!(
    project.guest_output("echo the session is still up"),
    "the session is still up",
    "and a service that is not running does not take the session with it"
  );

  let log = std::fs::read_to_string(project.session().ports_log()).unwrap_or_default();

  assert!(
    log.contains(&format!("nothing answers at 127.0.0.1:{port}")),
    "the relay writes what it could not reach where the terminal is Claude's: {log}"
  );
}

#[test]
fn undeclared_port_is_unreachable() {
  let service = Service::answering("the host");
  let project = Project::new("cbt-ports-none");

  let output = ask(&project, service.port, "hello");

  assert!(
    !output.status.success(),
    "a host port reaches the guest only because the manifest declared it"
  );
}
