//! What to say when something about a session is wrong.
//!
//! One submodule per subject: the engine, what is mounted, and the ways out of
//! the container. `diagnose` is the whole public surface — the checks stay
//! internal, so the order they run in is decided in one place.

mod engine;
mod host;
mod mounts;

use crate::session::Session;
use crate::session::credentials::CredentialSource;
use apple_container::engine::Engine;

pub use crate::doctor::engine::TESTED_CLI_VERSION;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
  Fail,
  Ok,
  Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
  pub detail: String,
  /// The findings behind `detail`, when a check is about several things at once
  /// — missing paths, dead symlinks, allowlisted commands. Kept out of the
  /// sentence: a reader scans them down a column, and comma-separated they are
  /// unreadable at the width a path already takes.
  pub items: Vec<String>,
  pub name: String,
  pub status: Status,
}

/// Every check, in a fixed order. `api_key_present` is passed in rather than
/// read here so the whole diagnosis is a pure function of its inputs.
pub fn diagnose(
  session: &Session,
  engine: &impl Engine,
  credentials: &impl CredentialSource,
  api_key_present: bool,
) -> Vec<Check> {
  let images = engine.images();

  vec![
    engine::cli_version(engine),
    engine::daemon(&images),
    engine::base_image(session, &images),
    mounts::mounted_paths(session),
    mounts::dangling_symlinks(session),
    mounts::live_mounts(session, engine),
    mounts::root_breadth(session),
    host::credentials(session, credentials, api_key_present),
    host::allowlist(session),
  ]
}

/// The one constructor, so every check reads as a name, a verdict, and a
/// sentence explaining it.
fn check(name: &str, status: Status, detail: impl Into<String>) -> Check {
  Check {
    detail: detail.into(),
    items: Vec::new(),
    name: name.to_string(),
    status,
  }
}

/// A check whose sentence is a heading over a list. The items print under the
/// sentence, not inside it, so it must say what they are without naming any.
fn listed(name: &str, status: Status, detail: impl Into<String>, items: Vec<String>) -> Check {
  Check {
    items,
    ..check(name, status, detail)
  }
}

/// Every case is driven through `diagnose` rather than the check it is about:
/// order and completeness are part of what `doctor` promises, and a test calling
/// one check directly would not notice it being dropped.
#[cfg(test)]
mod tests {
  use super::*;
  use crate::error::CredentialError;
  use crate::manifest::Manifest;
  use crate::session::credentials::CREDENTIALS_FILE_NAME;
  use crate::session::record::Record;
  use crate::workspace::paths::PathResolver;
  use apple_container::fake::RecordingEngine;
  use std::path::Path;
  use tempfile::TempDir;

  struct FakeSource(Option<String>);

  impl CredentialSource for FakeSource {
    fn read(&self) -> Result<Option<String>, CredentialError> {
      Ok(self.0.clone())
    }
  }

  fn in_keychain() -> FakeSource {
    FakeSource(Some("token".to_string()))
  }

  /// A session whose single root and Claude home both exist under `home`.
  fn session(home: &TempDir, roots: &str) -> Session {
    let base = home.path().canonicalize().expect("canonical temp");
    std::fs::create_dir_all(base.join("workspace")).expect("create root");
    let manifest: Manifest = toml::from_str(&format!(
      "[claude]\nhome = \"{}\"\n\n[project]\nname = \"cb\"\n\n[workspace]\nroots = [{roots}]\n",
      base.join("claude-home").display()
    ))
    .expect("manifest should parse");

    Session::new(
      manifest,
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    )
  }

  fn check<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks
      .iter()
      .find(|check| check.name == name)
      .unwrap_or_else(|| panic!("no {name:?} check in {checks:?}"))
  }

  fn quoted(path: &Path, suffix: &str) -> String {
    format!("\"{}\"", path.join(suffix).display())
  }

  /// The findings alone, for cases that only care a check named what it found.
  /// Layout is the CLI's business, so nothing here rebuilds a printed line.
  fn findings(check: &Check) -> String {
    check.items.join("\n")
  }

  #[test]
  fn reports_a_daemon_that_was_never_started() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::new(),
      &in_keychain(),
      false,
    );

    assert_eq!(check(&checks, "daemon").status, Status::Fail);
    assert!(
      check(&checks, "daemon")
        .detail
        .contains("container system start"),
      "detail should name the fix: {:?}",
      check(&checks, "daemon")
    );
    assert_eq!(
      check(&checks, "base image").status,
      Status::Fail,
      "an unreachable daemon cannot confirm the image"
    );
  }

  #[test]
  fn reports_a_missing_base_image() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["debian:stable-slim"]),
      &in_keychain(),
      false,
    );

    assert_eq!(check(&checks, "daemon").status, Status::Ok);
    assert_eq!(check(&checks, "base image").status, Status::Fail);
    assert!(
      check(&checks, "base image")
        .detail
        .contains("compostbin build"),
      "detail should name the fix: {:?}",
      check(&checks, "base image")
    );
  }

  #[test]
  fn passes_a_healthy_host() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    assert_eq!(
      checks
        .iter()
        .filter(|check| check.status != Status::Ok)
        .collect::<Vec<_>>(),
      Vec::<&Check>::new()
    );
    assert_eq!(check(&checks, "container CLI").detail, TESTED_CLI_VERSION);
  }

  #[test]
  fn names_a_mounted_path_that_does_not_exist() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "not-there")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let mounts = check(&checks, "mounted paths");
    assert_eq!(mounts.status, Status::Fail);
    assert!(
      findings(mounts).contains(&base.join("not-there").display().to_string()),
      "detail should name the missing path: {mounts:?}"
    );
  }

  #[test]
  fn warns_about_a_symlink_that_will_dangle_in_the_container() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    std::fs::create_dir_all(base.join("workspace/project")).expect("create project");
    std::fs::create_dir_all(base.join("elsewhere")).expect("create elsewhere");
    std::os::unix::fs::symlink(base.join("elsewhere"), base.join("workspace/project/vendor")).expect("create symlink");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let dangling = check(&checks, "dangling symlinks");
    assert_eq!(dangling.status, Status::Warn);
    assert!(
      findings(dangling).contains("vendor") && findings(dangling).contains("elsewhere"),
      "detail should name the link and where it points: {dangling:?}"
    );
  }

  #[test]
  fn warns_about_a_root_covering_the_whole_home_directory() {
    let home = TempDir::new().expect("temp dir");

    let checks = diagnose(
      &session(&home, "\"~\""),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let breadth = check(&checks, "mount breadth");
    assert_eq!(breadth.status, Status::Warn);
    assert!(
      findings(breadth).contains(
        &home
          .path()
          .canonicalize()
          .expect("canonical temp")
          .display()
          .to_string()
      ),
      "detail should name the broad root: {breadth:?}"
    );
  }

  #[test]
  fn accepts_an_api_key_when_the_keychain_is_empty() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &FakeSource(None),
      true,
    );

    assert_eq!(check(&checks, "credentials").status, Status::Ok);
    assert_eq!(check(&checks, "credentials").detail, "ANTHROPIC_API_KEY is set");
  }

  #[test]
  fn fails_when_there_is_no_way_to_authenticate() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &FakeSource(None),
      false,
    );

    assert_eq!(check(&checks, "credentials").status, Status::Fail);
  }

  #[test]
  fn accepts_a_token_already_seeded_into_claude_home() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    std::fs::create_dir_all(base.join("claude-home")).expect("create claude home");
    std::fs::write(base.join("claude-home").join(CREDENTIALS_FILE_NAME), "token").expect("write token");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &FakeSource(None),
      false,
    );

    assert_eq!(check(&checks, "credentials").status, Status::Ok);
  }

  #[test]
  fn warns_about_a_mounted_path_holding_credentials() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    let mut session = session(&home, &quoted(&base, "workspace"));
    session.manifest.paths.push(crate::manifest::PathEntry {
      readonly: true,
      source: base.join(".ssh").display().to_string(),
      target: None,
    });

    let checks = diagnose(
      &session,
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let breadth = check(&checks, "mount breadth");
    assert_eq!(breadth.status, Status::Warn);
    assert!(
      findings(breadth).contains(".ssh"),
      "detail should name the secret: {breadth:?}"
    );
  }

  #[test]
  fn warns_about_an_indirect_credential_path() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    std::fs::create_dir_all(base.join("workspace")).expect("create workspace");
    std::fs::create_dir_all(base.join(".ssh")).expect("create .ssh");
    std::os::unix::fs::symlink(base.join(".ssh"), base.join("keys")).expect("create symlink");
    let mut session = session(&home, &quoted(&base, "workspace"));
    for source in [base.join("workspace/../.ssh"), base.join("keys")] {
      session.manifest.paths.push(crate::manifest::PathEntry {
        readonly: true,
        source: source.display().to_string(),
        target: None,
      });
    }

    let checks = diagnose(
      &session,
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let breadth = check(&checks, "mount breadth");
    assert_eq!(breadth.status, Status::Warn);
    assert!(
      findings(breadth).contains("workspace/../.ssh") && findings(breadth).contains("keys"),
      "both routes to .ssh should be named: {breadth:?}"
    );
  }

  #[test]
  fn lists_every_command_the_guest_can_run_on_the_host() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    let mut session = session(&home, &quoted(&base, "workspace"));
    session.manifest.host = toml::from_str(
      "[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n\n[commands.test-one]\narguments = true\nargv = [\"cargo\", \"nextest\", \"run\"]\n",
    )
    .expect("host config should parse");

    let checks = diagnose(
      &session,
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let allowlist = check(&checks, "host commands");
    assert_eq!(
      allowlist.status,
      Status::Warn,
      "a command the guest may append arguments to deserves a warning: {allowlist:?}"
    );
    assert_eq!(
      allowlist.items,
      [
        "test = cargo nextest run",
        "test-one = cargo nextest run (+ guest arguments)"
      ],
      "one command per line, the widened one marked: {allowlist:?}"
    );
  }

  /// The session keeps the mount set it was created with, so something has to
  /// say the manifest moved on.
  #[test]
  fn warns_when_the_running_container_predates_a_manifest_change() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    let mut session = session(&home, &quoted(&base, "workspace"));
    Record::of(&session.mounts())
      .save(&session.mount_record())
      .expect("recording the started container should succeed");

    std::fs::create_dir_all(base.join("vendor")).expect("create vendor");
    session.manifest.paths.push(crate::manifest::PathEntry {
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });

    let checks = diagnose(
      &session,
      &RecordingEngine::with_containers(&[("compostbin-cb", true)]),
      &in_keychain(),
      false,
    );

    let mounts = check(&checks, "container mounts");
    assert_eq!(mounts.status, Status::Warn);
    assert!(
      findings(mounts).contains("vendor"),
      "detail should name the path that is not really mounted: {mounts:?}"
    );
    assert!(
      mounts.detail.contains("compostbin stop"),
      "the fix is not guessable, so it has to be printed: {mounts:?}"
    );
  }

  #[test]
  fn accepts_a_running_container_that_matches_the_manifest() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");
    let session = session(&home, &quoted(&base, "workspace"));
    Record::of(&session.mounts())
      .save(&session.mount_record())
      .expect("recording the started container should succeed");

    let checks = diagnose(
      &session,
      &RecordingEngine::with_containers(&[("compostbin-cb", true)]),
      &in_keychain(),
      false,
    );

    assert_eq!(check(&checks, "container mounts").status, Status::Ok);
  }

  /// Nothing has been started, so there is no mount set to disagree with, and a
  /// fresh checkout must not be told to stop a container that does not exist.
  #[test]
  fn says_nothing_is_running_rather_than_warning() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let mounts = check(&checks, "container mounts");
    assert_eq!(mounts.status, Status::Ok);
    assert!(mounts.detail.contains("not running"), "{mounts:?}");
  }

  #[test]
  fn warns_about_a_running_container_that_was_never_recorded() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_containers(&[("compostbin-cb", true)]),
      &in_keychain(),
      false,
    );

    let mounts = check(&checks, "container mounts");
    assert_eq!(
      mounts.status,
      Status::Warn,
      "an unrecorded container is one we can say nothing about: {mounts:?}"
    );
  }

  #[test]
  fn reports_no_host_channel_when_none_is_declared() {
    let home = TempDir::new().expect("temp dir");
    let base = home.path().canonicalize().expect("canonical temp");

    let checks = diagnose(
      &session(&home, &quoted(&base, "workspace")),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    assert_eq!(check(&checks, "host commands").status, Status::Ok);
  }
}
