#![cfg(feature = "integration")]
//! `[host] clipboard`: the guest's copying tools reaching the host pasteboard.
//!
//! The pasteboard is the developer's own, so every test here puts back what it
//! found. It is also the only shared thing these tests touch, so they run one
//! at a time.

use compostbin_core::manifest::CLIPBOARD_COMMAND;
use compostbin_core::session::CLIPBOARD_DISPLAY;
use compostbin_core::session::image::CLIPBOARD_TOOLS;
use compostbin_test::{Lock, Project, exclusive, stderr, stdout};
use std::io::Write;
use std::process::{Command, Stdio};

/// Holds the machine's one pasteboard for the length of a test, and puts back
/// what was on it. Held across processes, since that is how the tests run.
struct Pasteboard {
  /// Held only to be dropped, which is what lets the next test look.
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
  /// The lock is a field, so it is released after this: what was on the
  /// pasteboard is back before any other test can look at it.
  fn drop(&mut self) {
    copy(&self.restore);
  }
}

fn paste() -> String {
  let output = Command::new("pbpaste")
    .output()
    .expect("pbpaste should run");

  String::from_utf8_lossy(&output.stdout).into_owned()
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

/// One script under four names, each ahead of any real one on `PATH`, so a
/// project image that installs `xclip` still copies to the host.
#[test]
fn every_tool_reaches_the_pasteboard() {
  let project = with_clipboard("cbt-clipboard");
  let _pasteboard = Pasteboard::take();

  // One container for all four, stepping over its stdin: each tool copies,
  // says so, and waits while this side reads the pasteboard.
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
    String::from_utf8_lossy(&finished.stdout),
    stderr(&finished)
  );
}

/// The guest's copy crosses to the host as a command of its own, so it lands a
/// moment after the tool says it has run.
fn wait_for_the_pasteboard(expected: &str, tool: &str) {
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

  while paste() != expected {
    assert!(
      std::time::Instant::now() < deadline,
      "{tool} never reached the host pasteboard, which holds {:?}",
      paste()
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
}

/// Claude on Linux looks for `wl-copy` only when there is a display to copy to.
/// There is none; the variable is what makes it look.
#[test]
fn session_gets_a_display() {
  let project = with_clipboard("cbt-clipboard-display");

  assert_eq!(project.guest_output("echo $WAYLAND_DISPLAY"), CLIPBOARD_DISPLAY);
}

/// Write-only: nothing on the host pasteboard reaches the guest, and a paste
/// says so rather than returning something stale or empty.
#[test]
fn pasting_is_refused() {
  let _pasteboard = Pasteboard::take();
  let project = with_clipboard("cbt-clipboard-paste");
  copy("a host secret");

  // Not bare `xsel`, which pastes only when its stdin is a terminal and
  // copies otherwise — as it does here. One container for all of them: each
  // reports its own exit code.
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

/// Off by default: whatever the guest copies, the user may later paste into a
/// host terminal.
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

/// The clipboard is served as a host command, so a project that declares one
/// itself points the guest's tools at that instead.
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
