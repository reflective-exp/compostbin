#![cfg(feature = "integration")]
//! What compostbin does when the caller has a terminal to give.
//!
//! `exec.rs` and `host_commands.rs` assert the other direction — that `-t` is
//! refused without one, and that a `tty = true` command captured into a pipe
//! keeps its streams apart. Neither can say that a terminal asked for is a
//! terminal the guest, or the host command, actually gets: a test process's
//! streams are pipes. These run compostbin on a pty of their own instead.
//!
//! Everything a terminal shows arrives as one stream, the guest's own echo of
//! what was typed included, so these assert on what the output contains rather
//! than on what it is.

use compostbin_test::Project;

#[test]
fn exec_with_a_terminal_gives_the_guest_one() {
  let project = Project::new("cbt-tty-exec");

  let (shown, status) = project
    .on_a_terminal(&["exec", "-t", "sh", "-c", "test -t 0 && test -t 1 && tty"])
    .finish();

  assert!(status.success(), "the guest should have had a terminal: {shown}");
  assert!(
    shown.contains("/dev/pts/"),
    "the guest gets a pty of its own in the VM: {shown}"
  );
}

/// `compostbin shell` is `exec -t bash`, so it cannot run without a terminal
/// and nothing else in the suite has one to give it.
#[test]
fn shell_opens_bash_on_a_terminal() {
  let project = Project::new("cbt-tty-shell");
  let mut shell = project.on_a_terminal(&["shell"]);

  shell.send("echo \"opened $0 on $(tty)\"");
  shell.send("exit");

  let (shown, status) = shell.finish();

  assert!(status.success(), "the shell did not exit cleanly: {shown}");
  assert!(
    shown.contains("opened bash on /dev/pts/"),
    "shell is bash, on a pty: {shown}"
  );
}

/// And as whoever it was told to open as, which is the only flag `shell` has.
#[test]
fn shell_opens_as_the_user_it_was_given() {
  let project = Project::new("cbt-tty-shell-root");
  let mut shell = project.on_a_terminal(&["shell", "-U", "root"]);

  shell.send("echo \"opened as $(whoami)\"");
  shell.send("exit");

  let (shown, status) = shell.finish();

  assert!(status.success(), "the shell did not exit cleanly: {shown}");
  assert!(
    shown.contains("opened as root"),
    "`-U root` opens the shell as root: {shown}"
  );
}

/// A `tty = true` command gets a pty on the host only when the guest's caller
/// has a terminal to show it on. `host_commands.rs` can observe that condition
/// being false; this is the same manifest with it true.
#[test]
fn a_tty_host_command_gets_a_terminal_on_the_host() {
  let project = Project::new("cbt-tty-host");
  project.manifest(
    r#"
[host.commands.asked]
argv = ["sh", "-c", "test -t 1 && echo asked: terminal || echo asked: pipes"]
tty = true

[host.commands.did-not-ask]
argv = ["sh", "-c", "test -t 1 && echo plain: terminal || echo plain: pipes"]
"#,
  );

  let (shown, status) = project
    .on_a_terminal(&[
      "exec",
      "-t",
      "sh",
      "-c",
      "compostbin-host asked; compostbin-host did-not-ask",
    ])
    .finish();

  assert!(status.success(), "{shown}");
  assert!(
    shown.contains("asked: terminal"),
    "the command asked for a terminal and the caller had one to give: {shown}"
  );
  assert!(
    shown.contains("plain: pipes"),
    "a command that did not ask still gets pipes, terminal or not: {shown}"
  );
}
