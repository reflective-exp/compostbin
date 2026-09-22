#![cfg(feature = "integration")]
//! What the session is told about itself.
//!
//! The text is rendered on the host and asserted on by unit tests. What needs a
//! session is that it arrives: at the path Claude Code reads managed settings
//! from, as a hook the guest runs, and read-only — a guest that can rewrite its
//! own briefing has none.

use compostbin_core::manifest::Manifest;
use compostbin_core::session::briefing::{
  BRIEFING_FILE, MANAGED_SETTINGS_FILE, MANAGED_SETTINGS_TARGET, briefing, managed_settings,
};
use compostbin_test::{Project, stderr};

/// The briefing as the host would render it from the manifest the project has
/// on disk, which is what the guest must be holding.
fn rendered(project: &Project) -> String {
  briefing(&Manifest::load(&project.manifest_path()).expect("the manifest should load"))
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

  let seen = project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"));

  assert_eq!(
    seen,
    rendered(&project).trim(),
    "the briefing is generated from `[host.commands]`, so the two cannot drift"
  );
}

/// The briefing is a file a `SessionStart` hook prints, so Claude needs the
/// settings naming that hook, at the path it reads managed settings from.
#[test]
fn managed_settings_name_the_hook() {
  let project = Project::new("cbt-managed");

  assert_eq!(
    project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{MANAGED_SETTINGS_FILE}")),
    managed_settings().trim(),
    "Claude Code reads this path or nothing"
  );
  // The settings are only as good as the command they name, and the hook's
  // stdout is what reaches Claude's context.
  assert_eq!(
    project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}")),
    rendered(&project).trim()
  );
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

/// A project that declares nothing gets told that, rather than being left to
/// discover there is no way out.
#[test]
fn no_commands_is_said_too() {
  let project = Project::new("cbt-briefing-bare");

  let seen = project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"));

  assert!(seen.contains("declares no host commands"), "briefing: {seen}");
  assert_eq!(seen, rendered(&project).trim());
}

/// The briefing is written at creation, so an edited manifest reaches the guest
/// on the next container rather than the next `exec`.
#[test]
fn briefing_follows_the_manifest() {
  let project = Project::new("cbt-briefing-edit");

  let before = project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"));
  assert!(before.contains("declares no host commands"));

  project.manifest(
    r#"
[host.commands.build]
argv = ["make"]
"#,
  );

  let after = project.guest_output(&format!("cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"));

  assert!(
    after.contains("compostbin-host build  make"),
    "the next container is told what the manifest says now: {after}"
  );
  assert_eq!(after, rendered(&project).trim());
}
