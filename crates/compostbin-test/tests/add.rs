#![cfg(feature = "integration")]
//! `add` against a session that is already up.
//!
//! What a path resolves to, which file records it, and what `add` refuses are
//! decided on the host and tested there. What only a session can show is the
//! consequence the message promises: a container's mounts were fixed when it
//! was created, so an added path arrives in the next one, not this one.

use compostbin_core::manifest::MANIFEST_RELATIVE_PATH;
use compostbin_test::{Project, stderr, stdout};

#[test]
fn a_path_added_to_a_live_session_reaches_the_next_container() {
  let project = Project::new("cbt-add-live");
  let libfoo = project.home().join("libfoo");
  std::fs::create_dir(&libfoo).expect("create libfoo");
  std::fs::write(libfoo.join("marker"), "added mid-session\n").expect("write marker");

  // The owner reports what it can see before and after, stepped over its own
  // stdin so that `add` runs between the two.
  let mut owner = project.guest_in_background("ls /workspace > before && read step && ls /workspace > after");
  project.wait_for("before");

  let added = project.compostbin(&["add", &libfoo.display().to_string()]);

  assert!(added.status.success(), "add failed: {}", stderr(&added));
  assert!(
    stdout(&added).contains("exit the running session"),
    "add says what has to happen before the path is mounted: {}",
    stdout(&added)
  );

  owner.send("go");
  let finished = owner.finish();
  assert!(finished.status.success(), "{}", stderr(&finished));

  assert_eq!(
    project.read("after").trim(),
    "cbt-add-live",
    "the running container's mounts were fixed when it was created"
  );
  assert!(
    project.read(MANIFEST_RELATIVE_PATH).contains("libfoo"),
    "but the manifest records it, so the next session has it"
  );
  assert_eq!(
    project.guest_output("cat /workspace/libfoo/marker"),
    "added mid-session",
    "and the next container is the one that mounts it"
  );
}
