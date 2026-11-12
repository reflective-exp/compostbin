use std::io::{BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The host's end of the shell `guest/user-data.sh` leaves on `/dev/ttyV0.0`.
pub struct Agent {
  lines: Receiver<String>,
  input: PipeWriter,
  commands: u32,
}

pub struct Ran {
  pub code: i32,
  pub output: Vec<String>,
  pub seconds: f64,
}

impl Agent {
  pub fn new(output: PipeReader, input: PipeWriter) -> Self {
    let (sender, lines) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
      for line in BufReader::new(output).lines().map_while(Result::ok) {
        let line = line.trim_end_matches('\r').to_string();

        println!("guest| {line}");
        if sender.send(line).is_err() {
          break;
        }
      }
    });

    Self {
      lines,
      input,
      commands: 0,
    }
  }

  /// The first line starting with `prefix`, or `None` if none came within `timeout`.
  pub fn wait_for(&self, prefix: &str, timeout: Duration) -> Result<Option<String>, String> {
    self.wait_for_any(&[prefix], timeout)
  }

  /// The first line starting with any of `prefixes`, or `None` if none came within `timeout`.
  pub fn wait_for_any(&self, prefixes: &[&str], timeout: Duration) -> Result<Option<String>, String> {
    let started = Instant::now();

    loop {
      let Some(left) = timeout.checked_sub(started.elapsed()) else {
        return Ok(None);
      };

      match self.lines.recv_timeout(left) {
        Ok(line) if prefixes.iter().any(|prefix| line.starts_with(prefix)) => return Ok(Some(line)),
        Ok(_) => {}
        Err(RecvTimeoutError::Timeout) => return Ok(None),
        // As good as silence: a machine without the port closes it at once.
        Err(RecvTimeoutError::Disconnected) => {
          std::thread::sleep(left);
          return Ok(None);
        }
      }
    }
  }

  /// Sends a line without waiting for it to finish.
  pub fn send(&mut self, line: &str) -> Result<(), String> {
    writeln!(self.input, "{line}").map_err(|error| format!("writing to the guest: {error}"))
  }

  /// Runs `script` in the guest's shell, and collects what it printed.
  pub fn run(&mut self, script: &str, timeout: Duration) -> Result<Ran, String> {
    self.commands += 1;
    let marker = format!("spike-done-{}", self.commands);
    let started = Instant::now();

    // The marker starts a line of its own even after output without a newline;
    // the empty line that leaves otherwise is dropped below.
    write!(
      self.input,
      "{script}\nstatus=$?; printf '\\n{marker} %s\\n' \"$status\"\n"
    )
    .map_err(|error| format!("writing to the guest: {error}"))?;

    let mut output = Vec::new();
    loop {
      let left = timeout
        .checked_sub(started.elapsed())
        .ok_or_else(|| format!("{script:?} did not finish within {}s", timeout.as_secs()))?;

      match self.lines.recv_timeout(left) {
        Ok(line) => match line.strip_prefix(&marker) {
          Some(code) => {
            if output.last().is_some_and(String::is_empty) {
              output.pop();
            }
            return Ok(Ran {
              code: code.trim().parse().unwrap_or(-1),
              output,
              seconds: started.elapsed().as_secs_f64(),
            });
          }
          None => output.push(line),
        },
        Err(RecvTimeoutError::Timeout) => {
          return Err(format!("{script:?} did not finish within {}s", timeout.as_secs()));
        }
        Err(RecvTimeoutError::Disconnected) => return Err(format!("the port closed during {script:?}")),
      }
    }
  }

  /// As `run`, failing unless the script exits 0.
  pub fn check(&mut self, script: &str, timeout: Duration) -> Result<Ran, String> {
    let ran = self.run(script, timeout)?;

    if ran.code == 0 {
      Ok(ran)
    } else {
      Err(format!(
        "{script:?} exited {} after {:.1}s: {}",
        ran.code,
        ran.seconds,
        ran
          .output
          .iter()
          .rev()
          .take(20)
          .rev()
          .cloned()
          .collect::<Vec<_>>()
          .join("\n")
      ))
    }
  }
}
