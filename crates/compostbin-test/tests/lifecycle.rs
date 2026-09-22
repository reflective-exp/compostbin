#![cfg(feature = "integration")]
//! Who owns the container, and what `clean` may take while it is up.

use compostbin_core::session::CLAUDE_HOME_TARGET;
use compostbin_test::{Project, stderr, stdout};

/// Something container-local: `/tmp` is not mounted, so a file there says
/// which container a command ran in.
const LOCAL_MARKER: &str = "/tmp/joined";

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
    running_report(&project).contains("is running"),
    "the container is up while its creator is"
  );

  drop(running);

  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
  while running_report(&project).contains("is running") {
    assert!(
      std::time::Instant::now() < deadline,
      "the container outlived the command that created it"
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
  }
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
  wait_for(&project, "before");

  let cleaned = project.compostbin(&["clean"]);
  assert!(cleaned.status.success(), "clean failed: {}", stderr(&cleaned));
  assert!(
    project.state_dir().join("host/requests").is_dir(),
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
  let history = project.state_dir().join("claude-home/history");

  project.compostbin(&["clean"]);
  assert!(history.exists(), "`clean` is for transient state");

  let cleaned = project.compostbin(&["clean", "--all"]);

  assert!(cleaned.status.success(), "clean --all failed: {}", stderr(&cleaned));
  assert!(!history.exists(), "`--all` takes the Claude home with it");
}

/// What `doctor` says about the container, which is the only way to ask from
/// outside the process that owns it.
fn running_report(project: &Project) -> String {
  let report = stdout(&project.compostbin_with_env(&["doctor"], &[("ANTHROPIC_API_KEY", "k")]));

  report
    .lines()
    .find(|line| line.contains("container mounts"))
    .unwrap_or_default()
    .to_string()
}

fn wait_until_up(project: &Project) {
  let socket = project.state_dir().join("control.sock");
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

  while !socket.exists() {
    assert!(
      std::time::Instant::now() < deadline,
      "the session never came up: no socket at {}",
      socket.display()
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
}

/// Waits for a host command's output to land in the project.
///
/// Its contents, not its existence: the guest's `>` creates the file before the
/// command it redirects has run, so waiting to see it would clean the spool out
/// from under a request still in flight — leaving the guest client polling for a
/// status that will never be written.
fn wait_for(project: &Project, relative: &str) {
  let path = project.dir().join(relative);
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

  while !std::fs::read(&path).is_ok_and(|body| !body.is_empty()) {
    assert!(
      std::time::Instant::now() < deadline,
      "the guest never wrote {}",
      path.display()
    );
    std::thread::sleep(std::time::Duration::from_millis(50));
  }
}
