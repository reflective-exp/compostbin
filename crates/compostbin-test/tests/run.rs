#![cfg(feature = "integration")]

use compostbin_test::{Project, code, stderr, stdout};

/// `--version` because Claude Code runs it without an account, terminal, or
/// network.
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

fn version(shown: &str) -> Option<&str> {
  shown.split_whitespace().find(|word| {
    let parts: Vec<&str> = word.split('.').collect();

    parts.len() == 3
      && parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|digit| digit.is_ascii_digit()))
  })
}
