#![cfg(feature = "integration")]
//! `compostbin run`: the entrypoint Claude is attached to.
//!
//! Everything else here goes through `exec`, which is deliberately the
//! forgiving one — it may well be what is debugging a broken session. `run` is
//! the strict one, and the difference between them is only visible from here.

use compostbin_test::{Project, code, stderr, stdout};

/// Claude, with whatever followed `--`. `--version` because it is the one thing
/// Claude Code does without an account, a terminal, or a network: enough to say
/// that `run` attached Claude and that its arguments reached it.
#[test]
fn claude_is_attached_with_its_arguments() {
  let project = Project::new("cbt-run");

  let output = project.compostbin(&["run", "--", "--version"]);

  assert!(output.status.success(), "run failed: {}", stderr(&output));
  assert!(
    version(&stdout(&output)).is_some(),
    "`run -- --version` should have reached Claude: {}",
    stdout(&output)
  );
}

/// The other half of `container::failing_setup_still_allows_exec`: a session
/// whose setup failed is not the session that was asked for, so Claude is never
/// attached and the failing line's own status is what comes back.
#[test]
fn failing_setup_stops_claude() {
  let project = Project::new("cbt-run-setup-fails");
  project.manifest(
    r#"
[container]
setup = ["touch setup-ran", "exit 3"]
"#,
  );

  let output = project.compostbin(&["run", "--", "--version"]);

  assert_eq!(
    code(&output),
    3,
    "the failing line's status is the session's: {}",
    stderr(&output)
  );
  assert!(
    project.dir().join("setup-ran").exists(),
    "setup ran; it is the line after it that failed"
  );
  assert!(
    stderr(&output).contains("setup `exit 3` exited 3"),
    "the failure must be reported, not swallowed: {}",
    stderr(&output)
  );
  assert_eq!(
    version(&stdout(&output)),
    None,
    "Claude was never attached, so it printed no version: {}",
    stdout(&output)
  );
}

/// The first `<major>.<minor>.<patch>` in what Claude printed, which is all this
/// needs of a version it deliberately does not pin.
fn version(shown: &str) -> Option<&str> {
  shown.split_whitespace().find(|word| {
    let parts: Vec<&str> = word.split('.').collect();

    parts.len() == 3
      && parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|digit| digit.is_ascii_digit()))
  })
}
