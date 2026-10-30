#![cfg(feature = "integration")]

use compostbin_test::{Project, stderr, stdout};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use tempfile::TempDir;

struct Service {
  path: PathBuf,
  stop: Arc<AtomicBool>,
  thread: Option<JoinHandle<()>>,
}

impl Service {
  fn answering(dir: &Path, who: &'static str) -> Self {
    let path = dir.join(format!("{who}.sock"));
    let listener = UnixListener::bind(&path).expect("bind a host socket");
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
      path,
      stop,
      thread: Some(thread),
    }
  }
}

impl Drop for Service {
  fn drop(&mut self) {
    self.stop.store(true, Ordering::Relaxed);
    // Unblocks `incoming`, which is waiting rather than watching the flag.
    let _ = UnixStream::connect(&self.path);
    if let Some(thread) = self.thread.take() {
      let _ = thread.join();
    }
  }
}

/// `-t` keeps `socat` reading the answer after its own input ends.
fn exchange(name: &str, question: &str) -> String {
  format!("printf '{question}\\n' | socat -t 5 - UNIX-CONNECT:/run/compostbin/sockets/{name}")
}

fn declare(project: &Project, sockets: &[(&str, &Path)]) {
  let tables: String = sockets
    .iter()
    .map(|(name, path)| format!("[host.sockets.{name}]\nsource = \"{}\"\n\n", path.display()))
    .collect();
  project.manifest(&tables);
}

#[test]
fn declared_socket_reaches_the_host() {
  let dir = TempDir::new().expect("temp dir");
  let service = Service::answering(dir.path(), "host");
  let project = Project::new("cbt-sockets");
  declare(&project, &[("service", &service.path)]);

  let output = project.compostbin(&["exec", "sh", "-c", &exchange("service", "hello")]);

  assert!(
    output.status.success(),
    "the guest could not reach the socket: {}",
    stderr(&output)
  );
  assert_eq!(stdout(&output), "host says: hello");
}

#[test]
fn a_socket_answers_at_its_target_with_its_mode_for_the_sockets_group() {
  let dir = TempDir::new().expect("temp dir");
  let service = Service::answering(dir.path(), "host");
  let project = Project::new("cbt-sockets-target");
  project.manifest(&format!(
    "[host.sockets.service]\nsource = \"{}\"\ntarget = \"/run/deep/service.sock\"\nmode = \"0660\"\n",
    service.path.display()
  ));

  let output = project.compostbin(&[
    "exec",
    "sh",
    "-c",
    "stat -c '%G %a' /run/deep/service.sock && id -nG && printf 'hello\\n' | socat -t 5 - UNIX-CONNECT:/run/deep/service.sock",
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  let stdout = stdout(&output);
  let mut lines = stdout.lines();
  assert_eq!(lines.next(), Some("compostbin 660"), "{stdout}");
  assert!(
    lines
      .next()
      .is_some_and(|groups| groups.split(' ').any(|group| group == "compostbin")),
    "the session's user is in the sockets group: {stdout}"
  );
  assert_eq!(
    lines.next(),
    Some("host says: hello"),
    "and reaches the socket as it: {stdout}"
  );
}

#[test]
fn each_declared_socket_reaches_its_own_service() {
  let dir = TempDir::new().expect("temp dir");
  let first = Service::answering(dir.path(), "first");
  let second = Service::answering(dir.path(), "second");
  let project = Project::new("cbt-sockets-two");
  declare(&project, &[("first", &first.path), ("second", &second.path)]);

  // One `exec`, because each `exec` creates its own container.
  let output = project.compostbin(&[
    "exec",
    "sh",
    "-c",
    &format!("{}; echo; {}", exchange("first", "hello"), exchange("second", "hello")),
  ]);

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "first says: hello\nsecond says: hello");
}

#[test]
fn a_socket_with_nothing_behind_it_is_recorded_not_fatal() {
  let dir = TempDir::new().expect("temp dir");
  let missing = dir.path().join("missing.sock");
  let project = Project::new("cbt-sockets-dead");
  declare(&project, &[("missing", &missing)]);

  let unanswered = project.compostbin(&["exec", "sh", "-c", &exchange("missing", "anyone there")]);

  assert_eq!(stdout(&unanswered), "", "the relay has nothing to answer with");
  assert_eq!(
    project.guest_output("echo the session is still up"),
    "the session is still up"
  );

  let log = std::fs::read_to_string(project.session().ports_log()).unwrap_or_default();

  assert!(
    log.contains(&format!("nothing answers at {}", missing.display())),
    "{log}"
  );
}
