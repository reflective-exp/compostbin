#![cfg(feature = "integration")]

use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest};
use compostbin_core::session::briefing::{
  BRIEFING_FILE, MANAGED_SETTINGS_FILE, MANAGED_SETTINGS_TARGET, briefing, managed_settings,
};
use compostbin_test::{Project, stderr};

fn rendered(project: &Project) -> String {
  briefing(
    &Manifest::load(&project.manifest_path()).expect("the manifest should load"),
    MANIFEST_RELATIVE_PATH,
  )
}

fn seen(project: &Project) -> String {
  project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"))
}

#[test]
fn briefing_comes_from_the_manifest() {
  let project = Project::new("cbt-briefing");
  project.manifest(
    r#"
[host]
clipboard = true
ports = [7001]

[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

[host.commands.test-one]
arguments = true
argv = ["cargo", "nextest", "run"]
tty = true
"#,
  );

  assert_eq!(
    seen(&project),
    rendered(&project).trim(),
    "the briefing is generated from `[host.commands]`, so the two cannot drift"
  );
}

#[test]
fn managed_settings_name_the_hook() {
  let project = Project::new("cbt-managed");

  assert_eq!(
    project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{MANAGED_SETTINGS_FILE}")),
    managed_settings().trim(),
    "Claude Code reads this path or nothing"
  );
  assert_eq!(seen(&project), rendered(&project).trim());
}

#[test]
fn briefing_is_read_only() {
  let project = Project::new("cbt-managed-ro");

  let output = project.guest(&format!("echo nonsense > {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"));

  assert!(!output.status.success(), "managed settings are the point of managed");
  assert!(
    stderr(&output).contains("Read-only file system"),
    "stderr: {}",
    stderr(&output)
  );
}

#[test]
fn no_commands_is_said_too() {
  let project = Project::new("cbt-briefing-bare");

  let briefing = seen(&project);

  assert!(briefing.contains("declares no host commands"), "briefing: {briefing}");
  assert_eq!(briefing, rendered(&project).trim());
}

/// The briefing is written when the container is created, not on each `exec`.
#[test]
fn briefing_follows_the_manifest() {
  let project = Project::new("cbt-briefing-edit");

  let before = seen(&project);
  assert!(before.contains("declares no host commands"));

  project.manifest(
    r#"
[host.commands.build]
argv = ["make"]
"#,
  );

  let after = seen(&project);

  assert!(
    after.contains("compostbin-host build  make"),
    "the next container is told what the manifest says now: {after}"
  );
  assert_eq!(after, rendered(&project).trim());
}
