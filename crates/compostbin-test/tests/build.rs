#![cfg(feature = "integration")]

use compostbin_core::image;
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

#[test]
fn names_the_store_it_provisions() {
  let project = Project::new("cbt-build-store");

  let output = project.compostbin(&["build"]);
  let store = image::store(project.session().resolver());

  assert!(
    stdout(&output).contains(&format!("building compostbin/base:latest into {}", store.display())),
    "stdout: {}",
    stdout(&output)
  );
  for provisioned in ["state.json", "kernels", "initfs", "content"] {
    assert!(
      store.join(provisioned).exists(),
      "a provisioned store holds {provisioned}"
    );
  }
}

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
