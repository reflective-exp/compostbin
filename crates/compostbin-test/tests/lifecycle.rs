#![cfg(feature = "integration")]

use compostbin_core::host::Spool;
use compostbin_core::session::CLAUDE_HOME_TARGET;
use compostbin_test::{Project, poll_until, stderr, stdout};
use std::time::Duration;

/// `/tmp` is not mounted, so a file there identifies the container.
const LOCAL_MARKER: &str = "/tmp/joined";
const START_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// Ignored: a second command blocks until the owner's process exits and then
/// starts its own container, though `doctor` sees the session running throughout.
#[test]
#[ignore = "a second command waits for the owner's process instead of joining it"]
fn second_command_joins() {
  let project = Project::new("cbt-join");

  let owner = project.guest_in_background(&format!("sleep 3; cat {LOCAL_MARKER}"));
  wait_until_up(&project);

  project.guest_output(&format!("echo joined > {LOCAL_MARKER}"));

  assert_eq!(
    stdout(&owner.finish()).trim(),
    "joined",
    "the second command should have run in the container the first created"
  );
}

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

/// A running container's mount is bound to the spool's inode, so `clean` must
/// empty it in place.
#[test]
fn clean_keeps_the_channel_working() {
  let project = Project::new("cbt-clean-live");
  project.manifest(
    r#"
[host.commands.greet]
argv = ["echo", "still served"]
"#,
  );

  // Host commands read `/dev/null` because the client forwards its stdin and
  // would swallow the step line.
  let mut owner = project.guest_in_background(
    "compostbin-host greet > before < /dev/null \
     && read step \
     && compostbin-host greet > after < /dev/null",
  );
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
