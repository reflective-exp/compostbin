#![cfg(feature = "integration")]
//! `build`, against the store a session really boots from.
//!
//! Only the cheap half: that a store already provisioned is left alone and an
//! image already built is not built again. Building the base, or a project
//! image on top of it, takes minutes and leaves an image in the shared store
//! that nothing here could remove afterwards — so those are what
//! `compostbin build` is for, not what a test suite does on every run.

use compostbin_test::{Project, stderr, stdout};

#[test]
fn built_base_is_not_rebuilt() {
  let project = Project::new("cbt-build");

  let output = project.compostbin(&["build"]);

  assert!(output.status.success(), "build failed: {}", stderr(&output));
  assert!(
    stderr(&output).contains("is already built"),
    "a second build should be a no-op, not another Debian: {}",
    stderr(&output)
  );
}

/// The store is under `~/.cache` because everything in it is regenerable, and
/// `build` is what regenerates it — so it says which one it is filling.
#[test]
fn names_the_store_it_provisions() {
  let project = Project::new("cbt-build-store");

  let output = project.compostbin(&["build"]);
  let store = project.home().join(".cache/compostbin/images");

  assert!(
    stdout(&output).contains(&format!("building compostbin/base:latest into {}", store.display())),
    "stdout: {}",
    stdout(&output)
  );
  for provisioned in ["state.json", "kernels", "content"] {
    assert!(
      store.join(provisioned).exists(),
      "a provisioned store holds {provisioned}"
    );
  }
}

/// A project pointed at an image nobody built stops, naming the image, rather
/// than starting something else.
#[test]
fn unbuilt_image_stops_the_session() {
  let project = Project::new("cbt-build-missing");
  project.manifest(
    r#"
[project]
image = "compostbin/never-built:latest"
"#,
  );

  let output = project.guest("echo unreachable");

  assert!(!output.status.success(), "there is no image to start");
  assert!(
    stderr(&output).contains("compostbin/never-built:latest"),
    "the error should name the image it could not find: {}",
    stderr(&output)
  );
}
