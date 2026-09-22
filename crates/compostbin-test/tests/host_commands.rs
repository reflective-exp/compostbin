#![cfg(feature = "integration")]
//! The guest's way out to the host: `compostbin-host <name>`.
//!
//! Which requests the allowlist admits is decided by a pure function with
//! tests of its own. What only a session can show is the round trip: that the
//! guest script, the spool, the agent and the host process together behave as
//! though this shell had run the command — same streams, same status — and
//! that a refusal reaches the caller rather than being swallowed.

use compostbin_core::error::Refusal;
use compostbin_core::host::REJECTED_EXIT_CODE;
use compostbin_test::{Project, code, stderr, stdout};

/// A project whose host commands are shells: an allowlist entry is argv on the
/// host, so `sh -c` lets one test write what it wants run there.
fn with_host_commands(name: &str) -> Project {
  let project = Project::new(name);
  project.manifest(
    r#"
# Exact: the guest sends a name and gets this, whatever it asks for.
[host.commands.greet]
argv = ["sh", "-c", "echo hello from the host"]

[host.commands.fail]
argv = ["sh", "-c", "exit 17"]

[host.commands.both-streams]
argv = ["sh", "-c", "echo out; echo err >&2"]

[host.commands.swallow]
argv = ["cat"]

[host.commands.where]
argv = ["pwd"]

# Widened deliberately, as a test-one entry is.
[host.commands.repeat]
arguments = true
argv = ["echo"]
deny = ["--config", "-Z"]

# A terminal merges the streams, so this one only gets a pty when the caller
# has one to show it on.
[host.commands.terminal]
argv = ["sh", "-c", "echo out; echo err >&2"]
tty = true
"#,
  );

  project
}

#[test]
fn declared_command_runs_on_the_host() {
  let project = with_host_commands("cbt-host-runs");

  assert_eq!(project.guest_output("compostbin-host greet"), "hello from the host");
}

#[test]
fn exit_status_comes_back() {
  let project = with_host_commands("cbt-host-status");

  assert_eq!(
    code(&project.guest("compostbin-host fail")),
    17,
    "a failing build must fail in the guest too, or nothing notices"
  );
}

#[test]
fn streams_stay_separate() {
  let project = with_host_commands("cbt-host-streams");

  let output = project.guest("compostbin-host both-streams");

  assert_eq!(stdout(&output), "out\n");
  assert_eq!(stderr(&output), "err\n");
}

#[test]
fn stdin_reaches_the_command() {
  let project = with_host_commands("cbt-host-stdin");

  let output = project.guest("printf 'piped from the guest\\n' | compostbin-host swallow");

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "piped from the guest\n");
}

/// The command runs where the project is on the host, not where the guest is.
#[test]
fn runs_in_the_project_directory() {
  let project = with_host_commands("cbt-host-cwd");

  assert_eq!(
    project.guest_output("compostbin-host where"),
    project.dir().display().to_string(),
    "a build has to start where the project is"
  );
}

#[test]
fn undeclared_command_is_refused() {
  let project = with_host_commands("cbt-host-unknown");

  let output = project.guest("compostbin-host rm");

  assert_eq!(code(&output), REJECTED_EXIT_CODE);
  assert!(
    stderr(&output).contains(&Refusal::UnknownCommand("rm".to_string()).to_string()),
    "the guest should be told why: {}",
    stderr(&output)
  );
  assert_eq!(stdout(&output), "", "a refused command produces no output");
}

#[test]
fn arguments_need_opting_in() {
  let project = with_host_commands("cbt-host-arguments");

  let refused = project.guest("compostbin-host greet --version");

  assert_eq!(code(&refused), REJECTED_EXIT_CODE);
  assert!(
    stderr(&refused).contains(&Refusal::ArgumentsNotAllowed("greet".to_string()).to_string()),
    "stderr: {}",
    stderr(&refused)
  );

  assert_eq!(
    project.guest_output("compostbin-host repeat one two"),
    "one two",
    "a widened command appends what the guest sent"
  );
}

/// The deny list is what keeps a widened command from being pointed at other
/// code or configuration; the joined forms are the ones easy to miss.
#[test]
fn denied_arguments_are_refused() {
  let project = with_host_commands("cbt-host-denied");

  for argument in ["--config", "--config=other.toml", "-Z", "-Zbuild-std"] {
    let output = project.guest(&format!("compostbin-host repeat {argument}"));

    assert_eq!(code(&output), REJECTED_EXIT_CODE, "{argument} should be refused");
    assert!(
      stderr(&output).contains(&Refusal::DeniedArgument(argument.to_string()).to_string()),
      "{argument}: {}",
      stderr(&output)
    );
  }

  assert_eq!(
    project.guest_output("compostbin-host repeat --configured"),
    "--configured",
    "a denied prefix must not swallow an argument that merely starts the same"
  );
}

/// `tty = true` asks for a terminal; a caller capturing the output has none to
/// give, and must not get the merged streams a pty produces.
#[test]
fn a_tty_command_without_a_terminal_keeps_streams_apart() {
  let project = with_host_commands("cbt-host-tty");

  let output = project.guest("compostbin-host terminal");

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(stdout(&output), "out\n");
  assert_eq!(
    stderr(&output),
    "err\n",
    "a pty would have merged these into stdout, with colour codes besides"
  );
}

/// Subagents call the client independently, so more than one request is in the
/// spool at once.
#[test]
fn concurrent_requests_are_all_served() {
  let project = Project::new("cbt-host-concurrent");
  project.manifest(
    r#"
[host]
concurrency = 4

[host.commands.slow]
arguments = true
argv = ["sh", "-c", "sleep 0.5; echo served $1", "--"]
"#,
  );

  let output = project.guest("for name in a b c d; do compostbin-host slow $name & done; wait");

  assert!(output.status.success(), "{}", stderr(&output));
  let responses = stdout(&output);
  let mut served: Vec<&str> = responses.lines().collect();
  served.sort_unstable();
  assert_eq!(served, ["served a", "served b", "served c", "served d"]);
}

/// `concurrency` is a bound on what runs at once, not a promise to run that
/// many. `concurrent_requests_are_all_served` fires exactly as many as the cap
/// admits, so it never reaches it; a cap of one is the case where reaching it
/// is visible.
#[test]
fn the_cap_bounds_what_runs_at_once() {
  let project = Project::new("cbt-host-capped");
  project.manifest(
    r#"
[host]
concurrency = 1

[host.commands.step]
arguments = true
argv = ["sh", "-c", "echo in $1 >> log; sleep 0.4; echo out $1 >> log", "--"]
"#,
  );

  let output = project.guest("for name in a b c; do compostbin-host step $name & done; wait");

  assert!(output.status.success(), "{}", stderr(&output));

  let log = project.read("log");
  let steps: Vec<&str> = log.lines().collect();

  assert_eq!(steps.len(), 6, "each request runs, and runs once: {log}");
  for pair in steps.chunks(2) {
    assert_eq!(
      pair[0].strip_prefix("in "),
      pair.get(1).and_then(|line| line.strip_prefix("out ")),
      "a cap of one means one at a time, and these overlapped: {log}"
    );
  }
}

/// And a cap is a queue, not a limit on how much may be asked for: what does
/// not fit waits rather than being refused or dropped.
#[test]
fn requests_past_the_cap_wait_their_turn() {
  let project = Project::new("cbt-host-queued");
  project.manifest(
    r#"
[host]
concurrency = 2

[host.commands.step]
arguments = true
argv = ["sh", "-c", "echo $1 >> log", "--"]
"#,
  );

  let output = project.guest("for name in a b c d e f g h; do compostbin-host step $name & done; wait");

  assert!(output.status.success(), "{}", stderr(&output));

  let log = project.read("log");
  let mut served: Vec<&str> = log.lines().collect();
  served.sort_unstable();

  assert_eq!(
    served,
    ["a", "b", "c", "d", "e", "f", "g", "h"],
    "eight requests under a cap of two are all served: {log}"
  );
}

/// No `[host.commands]`, no channel: the spool is never created, and the
/// client says so rather than hanging on a directory nothing serves.
#[test]
fn no_commands_means_no_channel() {
  let project = Project::new("cbt-host-none");

  let output = project.guest("compostbin-host anything");

  assert_eq!(code(&output), 127, "the shell's \"no such command\"");
  assert!(
    stderr(&output).contains("[host.commands]"),
    "the guest should be told what to declare: {}",
    stderr(&output)
  );
  assert!(
    !project.state_dir().join("host/requests").exists(),
    "with no allowlist there is no spool to write requests into"
  );
}
