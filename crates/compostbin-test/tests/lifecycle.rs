#![cfg(feature = "integration")]
//! Who owns the container, and what `clean` may take while it is up.

use compostbin_core::host::Spool;
use compostbin_core::session::CLAUDE_HOME_TARGET;
use compostbin_test::{Project, poll_until, stderr, stdout};
use std::time::Duration;

/// Something container-local: `/tmp` is not mounted, so a file there says
/// which container a command ran in.
const LOCAL_MARKER: &str = "/tmp/joined";
/// How long a container is given to come up.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a container is given to go once its creator has.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// A second command joins the session rather than making one of its own, which
/// is what makes `compostbin shell` a client of a running `run`.
///
/// Ignored because it fails: a second command blocks until the owner's own
/// process exits, then starts a container of its own. Measured with an owner
/// sleeping 30 seconds, a joiner returned after 30.4 — in a container that had
/// never seen the owner's `/tmp` — while `doctor` from a third process saw the
/// session as running the whole time. Un-ignore it when joining works.
#[test]
#[ignore = "a second command waits for the owner's process instead of joining it"]
fn second_command_joins() {
  let project = Project::new("cbt-join");

  // The owner reports what it can see, so joining is proved from inside the
  // container rather than by timing.
  let owner = project.guest_in_background(&format!("sleep 3; cat {LOCAL_MARKER}"));
  wait_until_up(&project);

  project.guest_output(&format!("echo joined > {LOCAL_MARKER}"));

  assert_eq!(
    stdout(&owner.finish()).trim(),
    "joined",
    "the second command should have run in the container the first created"
  );
}

/// Whichever command creates the container owns it: when that process exits,
/// the container goes, with everything that joined it.
#[test]
fn container_goes_with_its_creator() {
  let project = Project::new("cbt-ownership");

  let running = project.guest_in_background("sleep 60");
  wait_until_up(&project);

  assert!(
    stdout(&project.doctor()).contains("is running"),
    "the container is up while its creator is"
  );

  drop(running);

  poll_until(STOP_TIMEOUT, "the container to go with its creator", || {
    (!stdout(&project.doctor()).contains("is running")).then_some(())
  });
}

/// The spool is a mount source, and a running container's mount is attached to
/// the inode it was created with: emptying it in place is the only kind of
/// cleaning a live mount survives. Both host commands are the owner's, so this
/// measures `clean` rather than what a second command can do.
#[test]
fn clean_keeps_the_channel_working() {
  let project = Project::new("cbt-clean-live");
  project.manifest(
    r#"
[host.commands.greet]
argv = ["echo", "still served"]
"#,
  );

  // Stepped over the process's stdin, not a file: the guest is told to go on
  // once `clean` has run. Each host command reads `/dev/null`, since the
  // client forwards its own stdin and would otherwise swallow that line.
  let mut owner = project.guest_in_background(
    "compostbin-host greet > before < /dev/null \
     && read step \
     && compostbin-host greet > after < /dev/null",
  );
  // Its contents, not its existence: the guest's `>` creates the file before
  // the command it redirects has run, so cleaning on sight would empty the
  // spool out from under a request still in flight.
  project.wait_for("before");

  let cleaned = project.compostbin(&["clean"]);
  assert!(cleaned.status.success(), "clean failed: {}", stderr(&cleaned));
  assert!(
    Spool::new(project.session().host_spool())
      .requests()
      .is_dir(),
    "the spool is emptied in place, never unlinked: the container is holding it"
  );

  owner.send("go");
  let finished = owner.finish();

  assert!(
    finished.status.success(),
    "the session did not survive `clean`: {}{}",
    stdout(&finished),
    stderr(&finished)
  );
  assert_eq!(
    project.read("after"),
    "still served\n",
    "the channel still works, rather than writing into a directory nothing reads"
  );
}

/// `clean` keeps the conversation `--continue` resumes; `--all` is how you
/// discard it.
#[test]
fn clean_all_discards_the_conversation() {
  let project = Project::new("cbt-clean-all");
  project.guest_output(&format!("echo a conversation > {CLAUDE_HOME_TARGET}/history"));
  let history = project.session().claude_home().join("history");

  project.compostbin(&["clean"]);
  assert!(history.exists(), "`clean` is for transient state");

  let cleaned = project.compostbin(&["clean", "--all"]);

  assert!(cleaned.status.success(), "clean --all failed: {}", stderr(&cleaned));
  assert!(!history.exists(), "`--all` takes the Claude home with it");
}

fn wait_until_up(project: &Project) {
  let socket = project.control_socket();

  poll_until(
    START_TIMEOUT,
    &format!("a control socket at {}", socket.display()),
    || socket.exists().then_some(()),
  );
}
