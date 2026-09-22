#![cfg(feature = "integration")]
//! `exec`, `shell` and what they hand the guest process.
//!
//! The contract is that running something in the session is as close to running
//! it here as a container allows: this side's streams are its streams, and its
//! exit status is ours. None of that can be seen from the host side alone.

use compostbin_test::{Project, code, stderr, stdout};

#[test]
fn exit_status_comes_back() {
  let project = Project::new("cbt-exit-status");

  assert_eq!(code(&project.guest("exit 0")), 0);
  assert_eq!(
    code(&project.guest("exit 42")),
    42,
    "an arbitrary failure comes back whole"
  );
  assert_eq!(
    code(&project.compostbin(&["exec", "false"])),
    1,
    "and so does an ordinary one"
  );
}

#[test]
fn streams_stay_separate() {
  let project = Project::new("cbt-streams");

  let output = project.guest("echo to stdout; echo to stderr >&2");

  assert_eq!(stdout(&output), "to stdout\n");
  assert_eq!(
    stderr(&output),
    "to stderr\n",
    "without a terminal the two must not be merged, or anything reading the output gets both"
  );
}

#[test]
fn stdin_reaches_the_guest() {
  let project = Project::new("cbt-stdin");

  let output = project.guest_with_input("cat", "a prompt piped in\n");

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    stdout(&output),
    "a prompt piped in\n",
    "`echo … | compostbin exec` has to work as written"
  );
}

#[test]
fn runs_as_claude_by_default() {
  let project = Project::new("cbt-default-user");

  assert_eq!(project.guest_output("id -un"), "claude");
}

/// For the `apt-cache search` case: another user of the image, on request.
#[test]
fn user_flag_picks_another_user() {
  let project = Project::new("cbt-user-flag");

  assert_eq!(
    stdout(&project.compostbin(&["exec", "-U", "root", "id", "-un"])).trim(),
    "root"
  );
  assert_eq!(
    stdout(&project.compostbin(&["exec", "--user", "root", "id", "-un"])).trim(),
    "root"
  );
}

/// Everything after the command belongs to the command, so a flag it shares
/// with `compostbin` is still its own.
#[test]
fn arguments_belong_to_the_command() {
  let project = Project::new("cbt-argv");

  let output = project.compostbin(&["exec", "echo", "-U", "--", "-t"]);

  assert_eq!(stdout(&output), "-U -- -t\n");
}

/// A terminal asked for is a terminal required: this side can only pass on one
/// it has, and a test process has none.
#[test]
fn a_terminal_is_refused_without_one() {
  let project = Project::new("cbt-no-terminal");

  for arguments in [vec!["exec", "-t", "true"], vec!["shell"]] {
    let output = project.compostbin(&arguments);

    assert_eq!(code(&output), 1, "{arguments:?} should not have run");
    assert!(
      stderr(&output).contains("needs a terminal on stdin and stdout"),
      "{arguments:?} should say why: {}",
      stderr(&output)
    );
  }
}

/// `exec` joins a session rather than needing one: with nothing running it
/// creates the container itself.
#[test]
fn exec_starts_the_container() {
  let project = Project::new("cbt-cold-start");

  assert!(
    !project.state_dir().join("control.sock").exists(),
    "nothing is running before the first command"
  );
  assert_eq!(project.guest_output("echo up"), "up");
}
