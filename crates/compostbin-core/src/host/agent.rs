//! Every output chunk is published by rename before `<id>.status` is written, so
//! a guest that sees the status has complete output.

use crate::error::{At, PathError};
use crate::host::pty::Pty;
use crate::host::request::{Request, Resolved, resolve};
use crate::host::spool::Spool;
use crate::host::{
  ERROR_STREAM, INPUT_EOF_SUFFIX, INPUT_SUFFIX, OUTPUT_STREAM, REJECTED_EXIT_CODE, SIGNAL_EXIT_BASE, TTY_SUFFIX,
};
use crate::manifest::HostCommand;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(100);

const CHUNK_SIZE: usize = 64 * 1024;

fn complete(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  id: &str,
  claimed: &Path,
) -> Result<(), PathError> {
  let status = run_claimed(spool, commands, project_dir, id, claimed)?;
  spool.write_status(id, status)?;
  std::fs::remove_file(claimed).at(claimed)
}

fn run_claimed(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  id: &str,
  claimed: &Path,
) -> Result<i32, PathError> {
  let text = std::fs::read_to_string(claimed).at(claimed)?;

  let Resolved { argv, tty } = match text
    .parse::<Request>()
    .and_then(|request| resolve(commands, &request))
  {
    Ok(resolved) => resolved,
    Err(refusal) => {
      spool.write_refusal(id, &refusal.to_string())?;
      return Ok(REJECTED_EXIT_CODE);
    }
  };

  let tty = tty && spool.responses().join(format!("{id}{TTY_SUFFIX}")).exists();

  let mut command = Command::new(&argv[0]);
  command.args(&argv[1..]).current_dir(project_dir);

  let started = if tty {
    start_on_terminal(&mut command)
  } else {
    start_on_pipes(&mut command)
  };

  let (mut child, stdin, sources) = match started {
    Ok(started) => started,
    Err(source) => {
      spool.write_refusal(id, &format!("{}: {source}", argv[0]))?;
      return Ok(REJECTED_EXIT_CODE);
    }
  };

  let finished = AtomicBool::new(false);

  // The scope joins every reader, so all chunks are published before the status file.
  let status = std::thread::scope(|scope| {
    scope.spawn(|| {
      if let Some(sink) = stdin {
        pump_stdin(spool, id, sink, &finished);
      }
    });

    for (stream, source) in sources {
      scope.spawn(move || publish_stream(spool, id, stream, source));
    }

    let status = child.wait();
    finished.store(true, Ordering::Relaxed);
    status
  });

  status
    .map(|status| {
      status
        .code()
        .unwrap_or_else(|| SIGNAL_EXIT_BASE + status.signal().unwrap_or_default())
    })
    .at(&argv[0])
}

type Started = (
  Child,
  Option<Box<dyn Write + Send>>,
  Vec<(&'static str, Box<dyn Read + Send>)>,
);

fn start_on_pipes(command: &mut Command) -> io::Result<Started> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

  let mut child = command.spawn()?;
  let stdin = child
    .stdin
    .take()
    .map(|sink| Box::new(sink) as Box<dyn Write + Send>);

  let mut sources: Vec<(&'static str, Box<dyn Read + Send>)> = Vec::new();
  if let Some(output) = child.stdout.take() {
    sources.push((OUTPUT_STREAM, Box::new(output)));
  }
  if let Some(errors) = child.stderr.take() {
    sources.push((ERROR_STREAM, Box::new(errors)));
  }

  Ok((child, stdin, sources))
}

fn start_on_terminal(command: &mut Command) -> io::Result<Started> {
  let terminal = Pty::open()?;
  terminal.attach(command)?;

  let child = command.spawn()?;
  // Dropping our slave is what lets master reads end when the command exits.
  let master = terminal.into_master();
  let reader = File::from(master.try_clone()?);

  Ok((
    child,
    Some(Box::new(File::from(master))),
    vec![(OUTPUT_STREAM, Box::new(reader))],
  ))
}

/// Ends on a read error too, since that is how a pty master reports the command exited.
fn publish_stream(spool: &Spool, id: &str, stream: &str, mut source: impl Read) {
  let mut buffer = vec![0; CHUNK_SIZE];
  let mut sequence = 0;

  loop {
    match source.read(&mut buffer) {
      Ok(0) | Err(_) => return,
      Ok(read) => {
        sequence += 1;
        let _ = spool.publish_chunk(id, stream, sequence, &buffer[..read]);
      }
    }
  }
}

/// The EOF marker is checked before reading, so the final read sees everything
/// written before it appeared.
fn pump_stdin(spool: &Spool, id: &str, mut sink: Box<dyn Write + Send>, finished: &AtomicBool) {
  let input_path = spool.responses().join(format!("{id}{INPUT_SUFFIX}"));
  let eof_path = spool.responses().join(format!("{id}{INPUT_EOF_SUFFIX}"));
  let mut offset = 0;
  let mut buffer = Vec::new();

  loop {
    let ended = eof_path.exists() || finished.load(Ordering::Relaxed);

    // Reopened every poll because a descriptor held across the mount can miss the guest's appends.
    if let Ok(mut file) = File::open(&input_path) {
      buffer.clear();
      if file.seek(SeekFrom::Start(offset)).is_ok() && file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
        offset += buffer.len() as u64;
        if sink.write_all(&buffer).is_err() || sink.flush().is_err() {
          return;
        }
      }
    }

    if ended {
      return;
    }

    std::thread::sleep(POLL_INTERVAL);
  }
}

pub fn serve(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  limit: usize,
  stop: &AtomicBool,
) -> Result<(), PathError> {
  let in_flight = AtomicUsize::new(0);

  std::thread::scope(|scope| {
    while !stop.load(Ordering::Relaxed) {
      if in_flight.load(Ordering::Relaxed) >= limit.max(1) {
        std::thread::sleep(POLL_INTERVAL);
        continue;
      }

      match spool.claim_next()? {
        None => std::thread::sleep(POLL_INTERVAL),
        Some((id, claimed)) => {
          in_flight.fetch_add(1, Ordering::Relaxed);
          let in_flight = &in_flight;

          scope.spawn(move || {
            if let Err(error) = complete(spool, commands, project_dir, &id, &claimed) {
              eprintln!("compostbin: serving {id}: {error}");
            }

            in_flight.fetch_sub(1, Ordering::Relaxed);
          });
        }
      }
    }

    Ok(())
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::fixtures::{allowlist, commands, spool, terminal_command};
  use crate::host::{PARTIAL_SUFFIX, STATUS_SUFFIX};
  use std::path::PathBuf;
  use tempfile::TempDir;

  fn serve_once(
    spool: &Spool,
    commands: &BTreeMap<String, HostCommand>,
    project_dir: &Path,
  ) -> Result<bool, PathError> {
    let Some((id, claimed)) = spool.claim_next()? else {
      return Ok(false);
    };

    complete(spool, commands, project_dir, &id, &claimed)?;
    Ok(true)
  }

  fn serve_one(spool: &Spool, allowlist: &BTreeMap<String, HostCommand>, project_dir: &Path, id: &str) -> Response {
    assert!(
      serve_once(spool, allowlist, project_dir).expect("serve should succeed"),
      "there should have been a request to serve"
    );

    let status = std::fs::read_to_string(spool.responses().join(format!("{id}{STATUS_SUFFIX}"))).unwrap_or_default();

    Response {
      errors: gather(spool, id, ERROR_STREAM),
      output: gather(spool, id, OUTPUT_STREAM),
      status: status.trim().parse().expect("status should be a number"),
    }
  }

  fn gather(spool: &Spool, id: &str, stream: &str) -> String {
    let prefix = format!("{id}.{stream}.");
    let mut chunks: Vec<PathBuf> = std::fs::read_dir(spool.responses())
      .expect("responses should be readable")
      .filter_map(|entry| entry.ok().map(|entry| entry.path()))
      .filter(|path| {
        path
          .file_name()
          .and_then(|name| name.to_str())
          .is_some_and(|name| name.starts_with(&prefix) && !name.ends_with(PARTIAL_SUFFIX))
      })
      .collect();

    chunks.sort();
    chunks
      .iter()
      .map(|path| std::fs::read_to_string(path).unwrap_or_default())
      .collect()
  }

  fn caller_has_terminal(spool: &Spool, id: &str) {
    std::fs::write(spool.responses().join(format!("{id}{TTY_SUFFIX}")), "").expect("write tty marker");
  }

  #[derive(Debug, PartialEq)]
  struct Response {
    errors: String,
    output: String,
    status: i32,
  }

  #[test]
  fn reports_nothing_to_do_on_an_empty_spool() {
    let (_temp, spool) = spool();

    assert!(!serve_once(&spool, &allowlist(), Path::new(".")).expect("serve should succeed"));
  }

  #[test]
  fn runs_an_allowlisted_command_and_reports_its_output_and_status() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001"),
      Response {
        errors: String::new(),
        output: "hello\n".to_string(),
        status: 0,
      }
    );
  }

  #[test]
  fn reports_the_exit_code_of_a_failing_command() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("fail", &["sh", "-c", "exit 3"], false)]);

    spool
      .submit("0001", &Request::new("fail", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").status, 3);
  }

  #[test]
  fn reports_a_refusal_as_output_and_a_status() {
    let (_temp, spool) = spool();

    spool
      .submit("0001", &Request::new("rm", Vec::new()))
      .expect("submit should succeed");

    let response = serve_one(&spool, &allowlist(), Path::new("."), "0001");

    assert_eq!(response.status, REJECTED_EXIT_CODE);
    assert!(
      response.errors.contains("rm"),
      "{} should name the refused command",
      response.errors
    );
  }

  #[test]
  fn runs_the_command_in_the_project_directory() {
    let (_temp, spool) = spool();
    let project = TempDir::new().expect("project dir");
    let project_dir = project.path().canonicalize().expect("canonical project");
    let allowlist = commands(&[("where", &["pwd"], false)]);

    spool
      .submit("0001", &Request::new("where", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(
      serve_one(&spool, &allowlist, &project_dir, "0001")
        .output
        .trim(),
      project_dir.display().to_string()
    );
  }

  #[test]
  fn serves_the_oldest_request_first() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[
      ("first", &["echo", "first"], false),
      ("second", &["echo", "second"], false),
    ]);

    spool
      .submit("0002", &Request::new("second", Vec::new()))
      .expect("submit");
    spool
      .submit("0001", &Request::new("first", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "first\n");
    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0002").output, "second\n");
  }

  #[test]
  fn ignores_a_request_that_is_still_being_written() {
    let (_temp, spool) = spool();
    std::fs::write(spool.requests().join(format!("0001{PARTIAL_SUFFIX}")), "greet\n").expect("write partial");

    assert!(!serve_once(&spool, &allowlist(), Path::new(".")).expect("serve should succeed"));
  }

  #[test]
  fn keeps_stdout_and_stderr_apart() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("both", &["sh", "-c", "echo out; echo err >&2"], false)]);

    spool
      .submit("0001", &Request::new("both", Vec::new()))
      .expect("submit");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001"),
      Response {
        errors: "err\n".to_string(),
        output: "out\n".to_string(),
        status: 0,
      }
    );
  }

  #[test]
  fn gives_a_terminal_to_a_command_that_asked_for_one() {
    let (_temp, spool) = spool();
    let allowlist = terminal_command("interactive", &["sh", "-c", "test -t 1 && test -t 0"]);

    caller_has_terminal(&spool, "0001");
    spool
      .submit("0001", &Request::new("interactive", Vec::new()))
      .expect("submit");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001").status,
      0,
      "both the command's input and its output should be the terminal"
    );
  }

  #[test]
  fn gives_pipes_to_a_terminal_command_whose_caller_has_none() {
    let (_temp, spool) = spool();
    let allowlist = terminal_command("interactive", &["sh", "-c", "echo out; echo err >&2; test -t 1"]);

    spool
      .submit("0001", &Request::new("interactive", Vec::new()))
      .expect("submit");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001"),
      Response {
        errors: "err\n".to_string(),
        output: "out\n".to_string(),
        status: 1,
      },
      "no terminal, and the streams kept apart"
    );
  }

  #[test]
  fn gives_pipes_to_a_command_that_did_not() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("plain", &["sh", "-c", "test -t 1"], false)]);

    spool
      .submit("0001", &Request::new("plain", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").status, 1);
  }

  #[test]
  fn merges_the_streams_a_terminal_cannot_keep_apart() {
    let (_temp, spool) = spool();
    let allowlist = terminal_command("both", &["sh", "-c", "printf out; printf err >&2"]);

    caller_has_terminal(&spool, "0001");
    spool
      .submit("0001", &Request::new("both", Vec::new()))
      .expect("submit");

    let response = serve_one(&spool, &allowlist, Path::new("."), "0001");

    assert_eq!(response.status, 0);
    assert_eq!(
      response.errors,
      String::new(),
      "a pty has no second descriptor to publish: {response:?}"
    );
    assert!(
      response.output.contains("out") && response.output.contains("err"),
      "both streams should arrive on the terminal: {response:?}"
    );
  }

  /// Closing a pty master is not EOF to the slave, so this command must exit after one line.
  #[test]
  fn feeds_a_terminal_command_its_input() {
    let (_temp, spool) = spool();
    let allowlist = terminal_command("echo-line", &["sh", "-c", "read line; printf '%s' \"$line\""]);

    std::fs::write(spool.responses().join(format!("0001{INPUT_SUFFIX}")), "hello\n").expect("write input");
    std::fs::write(spool.responses().join(format!("0001{INPUT_EOF_SUFFIX}")), "").expect("mark end");
    caller_has_terminal(&spool, "0001");
    spool
      .submit("0001", &Request::new("echo-line", Vec::new()))
      .expect("submit");

    assert!(
      serve_one(&spool, &allowlist, Path::new("."), "0001")
        .output
        .contains("hello"),
      "a pty echoes the input back too, so the reply is what matters, not the whole stream"
    );
  }

  #[test]
  fn feeds_the_command_its_input() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("upper", &["tr", "a-z", "A-Z"], false)]);

    std::fs::write(spool.responses().join(format!("0001{INPUT_SUFFIX}")), "hello\n").expect("write input");
    std::fs::write(spool.responses().join(format!("0001{INPUT_EOF_SUFFIX}")), "").expect("mark end");
    spool
      .submit("0001", &Request::new("upper", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "HELLO\n");
  }

  #[test]
  fn finishes_a_command_that_ignores_unterminated_input() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    std::fs::write(spool.responses().join(format!("0001{INPUT_SUFFIX}")), "ignored").expect("write input");
    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "hello\n");
  }

  #[test]
  fn reassembles_output_spanning_many_chunks() {
    let (_temp, spool) = spool();
    let lines = 50_000;
    let allowlist = commands(&[("many", &["seq", "1", &lines.to_string()], false)]);

    spool
      .submit("0001", &Request::new("many", Vec::new()))
      .expect("submit");
    let response = serve_one(&spool, &allowlist, Path::new("."), "0001");

    let collected: Vec<&str> = response.output.lines().collect();
    assert_eq!(collected.len(), lines, "every line should survive reassembly");
    assert_eq!(collected.first(), Some(&"1"), "and they should be in order");
    assert_eq!(collected.last(), Some(&"50000"));

    let chunks = std::fs::read_dir(spool.responses())
      .expect("responses")
      .filter(|entry| {
        entry.as_ref().is_ok_and(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .contains(&format!(".{OUTPUT_STREAM}."))
        })
      })
      .count();
    assert!(
      chunks > 1,
      "output this size should have taken more than one chunk, got {chunks}"
    );
  }

  #[test]
  fn leaves_no_partial_chunk_behind() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit should succeed");
    serve_once(&spool, &allowlist, Path::new(".")).expect("serve should succeed");

    let partials = std::fs::read_dir(spool.responses())
      .expect("responses")
      .filter(|entry| {
        entry.as_ref().is_ok_and(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .ends_with(PARTIAL_SUFFIX)
        })
      })
      .count();
    assert_eq!(partials, 0);
  }

  #[test]
  fn leaves_no_claimed_request_behind() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");
    serve_once(&spool, &allowlist, Path::new(".")).expect("serve should succeed");

    assert_eq!(
      std::fs::read_dir(spool.running())
        .expect("running dir")
        .count(),
      0,
      "the claimed request should have been removed"
    );
  }
}
