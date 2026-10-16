#![cfg(feature = "integration")]

use compostbin_core::host::CLIPBOARD_COMMAND;
use compostbin_core::image::CLIPBOARD_TOOLS;
use compostbin_core::session::CLIPBOARD_DISPLAY;
use compostbin_test::{Lock, Project, exclusive, poll_until, stderr, stdout};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

const COPY_TIMEOUT: Duration = Duration::from_secs(30);

struct Pasteboard {
  _held: Lock,
  restore: String,
}

impl Pasteboard {
  fn take() -> Self {
    Self {
      _held: exclusive("pasteboard"),
      restore: paste(),
    }
  }
}

impl Drop for Pasteboard {
  /// Runs before the lock field is dropped, so the pasteboard is restored first.
  fn drop(&mut self) {
    copy(&self.restore);
  }
}

fn paste() -> String {
  let output = Command::new("pbpaste")
    .output()
    .expect("pbpaste should run");

  stdout(&output)
}

fn copy(contents: &str) {
  let mut pbcopy = Command::new("pbcopy")
    .stdin(Stdio::piped())
    .spawn()
    .expect("pbcopy should run");
  pbcopy
    .stdin
    .take()
    .expect("piped stdin")
    .write_all(contents.as_bytes())
    .expect("write to the pasteboard");
  pbcopy.wait().expect("pbcopy should finish");
}

fn with_clipboard(name: &str) -> Project {
  let project = Project::new(name);
  project.manifest(
    r#"
[host]
clipboard = true
"#,
  );

  project
}

#[test]
fn every_tool_reaches_the_pasteboard() {
  let project = with_clipboard("cbt-clipboard");
  let _pasteboard = Pasteboard::take();

  let copying: Vec<String> = CLIPBOARD_TOOLS
    .iter()
    .map(|tool| format!("printf '%s' 'copied with {tool}' | {tool} && read step"))
    .collect();
  let mut guest = project.guest_in_background(&copying.join("; "));

  for tool in CLIPBOARD_TOOLS {
    wait_for_the_pasteboard(&format!("copied with {tool}"), tool);
    guest.send("next");
  }

  let finished = guest.finish();
  assert!(
    finished.status.success(),
    "a copying tool failed: {}{}",
    stdout(&finished),
    stderr(&finished)
  );
}

fn wait_for_the_pasteboard(expected: &str, tool: &str) {
  poll_until(COPY_TIMEOUT, &format!("{tool} to reach the host pasteboard"), || {
    (paste() == expected).then_some(())
  });
}

/// Claude on Linux looks for `wl-copy` only when `WAYLAND_DISPLAY` is set.
#[test]
fn session_gets_a_display() {
  let project = with_clipboard("cbt-clipboard-display");

  assert_eq!(project.guest_output("echo $WAYLAND_DISPLAY"), CLIPBOARD_DISPLAY);
}

#[test]
fn pasting_is_refused() {
  let _pasteboard = Pasteboard::take();
  let project = with_clipboard("cbt-clipboard-paste");
  copy("a host secret");

  // Not bare `xsel`, which copies rather than pastes when stdin is not a terminal.
  let attempts = ["xclip -o", "xclip -out", "xsel -o", "xsel --output"];
  let output = project.guest(
    &attempts
      .iter()
      .map(|attempt| format!("{attempt}; echo \"{attempt} exited $?\""))
      .collect::<Vec<_>>()
      .join("; "),
  );
  let reported = stdout(&output);

  for attempt in attempts {
    assert!(
      reported.contains(&format!("{attempt} exited 1")),
      "`{attempt}` should have been refused:\n{reported}"
    );
  }
  assert_eq!(
    stderr(&output)
      .lines()
      .filter(|line| line.contains("write-only"))
      .count(),
    attempts.len(),
    "each attempt should say why: {}",
    stderr(&output)
  );
  assert!(
    !reported.contains("a host secret"),
    "nothing on the host pasteboard may reach the guest:\n{reported}"
  );
}

#[test]
fn off_without_the_setting() {
  let _pasteboard = Pasteboard::take();
  let project = Project::new("cbt-clipboard-off");
  copy("untouched");

  let output = project.guest("printf 'from the guest' | pbcopy");

  assert!(
    !output.status.success(),
    "with no host channel there is nothing to serve it"
  );
  assert_eq!(paste(), "untouched", "and the host pasteboard is not written to");
}

#[test]
fn declared_command_wins() {
  let project = Project::new("cbt-clipboard-own");
  let recorded = project.dir().join("copied");
  project.manifest(&format!(
    r#"
[host]
clipboard = true

[host.commands.{CLIPBOARD_COMMAND}]
argv = ["sh", "-c", "cat > {}"]
"#,
    recorded.display()
  ));

  let output = project.guest("printf 'to the project' | wl-copy");

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    std::fs::read_to_string(&recorded).expect("the declared command should have run"),
    "to the project"
  );
}
