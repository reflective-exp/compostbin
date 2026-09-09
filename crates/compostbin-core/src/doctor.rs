use crate::credentials::{CREDENTIALS_FILE_NAME, CredentialSource, KEYCHAIN_SERVICE};
use crate::mounts::Record;
use crate::paths::{Danger, danger};
use crate::session::Session;
use crate::workspace::WALK_LIMIT;
use apple_container::engine::Engine;
use apple_container::error::EngineError;

/// The `container` CLI version every fact in the plan was measured against.
pub const TESTED_CLI_VERSION: &str = "1.3.1";
/// Roots so broad that mounting them hands the container the whole account.
/// Kept as a re-export of the list `add` refuses on, so the two cannot drift.
pub use crate::paths::BROAD_PATHS as BROAD_ROOTS;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
  Fail,
  Ok,
  Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
  pub detail: String,
  pub name: String,
  pub status: Status,
}

/// Every §7 check, in a fixed order. `api_key_present` is passed in rather than
/// read here so the whole diagnosis is a pure function of its inputs.
pub fn diagnose(
  session: &Session,
  engine: &impl Engine,
  credentials: &impl CredentialSource,
  api_key_present: bool,
) -> Vec<Check> {
  let images = engine.images();

  vec![
    cli_version(engine),
    daemon(&images),
    base_image(session, &images),
    mounted_paths(session),
    dangling_symlinks(session),
    live_mounts(session, engine),
    root_breadth(session),
    credentials_check(session, credentials, api_key_present),
    host_allowlist(session),
  ]
}

fn check(name: &str, status: Status, detail: impl Into<String>) -> Check {
  Check {
    detail: detail.into(),
    name: name.to_string(),
    status,
  }
}

fn cli_version(engine: &impl Engine) -> Check {
  match engine.version() {
    Ok(Some(version)) if version == TESTED_CLI_VERSION => check("container CLI", Status::Ok, version),
    Ok(Some(version)) => check(
      "container CLI",
      Status::Warn,
      format!("{version}; every fact in the plan was measured on {TESTED_CLI_VERSION}"),
    ),
    Ok(None) => check("container CLI", Status::Warn, "unrecognised version output"),
    Err(error) => check("container CLI", Status::Fail, error.to_string()),
  }
}

fn daemon(images: &Result<Vec<String>, EngineError>) -> Check {
  match images {
    Ok(_) => check("daemon", Status::Ok, "responding"),
    Err(error) => check("daemon", Status::Fail, format!("{error}; run `container system start`")),
  }
}

fn base_image(session: &Session, images: &Result<Vec<String>, EngineError>) -> Check {
  let wanted = &session.manifest.project.image;

  match images {
    Err(_) => check(
      "base image",
      Status::Fail,
      format!("cannot look for {wanted} while the daemon is unreachable"),
    ),
    Ok(images) if images.contains(wanted) => check("base image", Status::Ok, wanted),
    Ok(_) => check(
      "base image",
      Status::Fail,
      format!("{wanted} is not built; run `compostbin build`"),
    ),
  }
}

/// Roots and explicit paths only. Claude's home is deliberately excluded: `run`
/// creates it, so its absence before the first session is normal.
fn mounted_paths(session: &Session) -> Check {
  let declared = session
    .manifest
    .workspace
    .roots
    .iter()
    .chain(session.manifest.paths.iter().map(|entry| &entry.source));
  let missing: Vec<String> = declared
    .map(|raw| session.resolve(raw))
    .filter(|path| !path.exists())
    .map(|path| path.display().to_string())
    .collect();

  if missing.is_empty() {
    return check("mounted paths", Status::Ok, "every declared path exists");
  }

  check(
    "mounted paths",
    Status::Fail,
    format!("missing on the host: {}", missing.join(", ")),
  )
}

/// The likeliest silent failure of the whole design: a symlink that resolves
/// on the host and dangles in the container, because its target is outside every
/// mounted tree (F5). Nothing reports it — the file is simply not there.
///
/// A warning rather than a failure: the session runs, and the fix is either to
/// mount the target or to stop relying on the link, both of which are the user's
/// call.
fn dangling_symlinks(session: &Session) -> Check {
  const NAMED: usize = 5;

  let found = session.workspace().escaping_symlinks(WALK_LIMIT);

  if found.escapes.is_empty() {
    let detail = if found.exhausted {
      format!("none in the first {WALK_LIMIT} entries; the mounted trees are too large to walk in full")
    } else {
      "no symlink escapes the mounted trees".to_string()
    };

    return check("dangling symlinks", Status::Ok, detail);
  }

  let named: Vec<String> = found
    .escapes
    .iter()
    .take(NAMED)
    .map(|escape| format!("{} -> {}", escape.link.display(), escape.target.display()))
    .collect();
  let rest = found.escapes.len().saturating_sub(named.len());
  let more = if rest > 0 {
    format!(" (and {rest} more)")
  } else {
    String::new()
  };

  check(
    "dangling symlinks",
    Status::Warn,
    format!(
      "dead in the container, because the target is not mounted: {}{more}",
      named.join(", ")
    ),
  )
}

/// Whether the container that is running now has the mounts the manifest
/// describes. `run` attaches to a live container rather than recreating it, and
/// F11 forbids adding mounts to one, so a manifest edited mid-session takes
/// effect at the next `stop` + `run` and not before. Nothing else says so: the
/// path is simply missing in the guest, which reads as the feature being broken.
///
/// A warning, not a failure — the session works, and recreating the container is
/// the user's call — but the *fix* is named, because it is not guessable.
fn live_mounts(session: &Session, engine: &impl Engine) -> Check {
  let name = session.container_name();

  let running = match engine.running_containers() {
    Ok(running) => running,
    Err(error) => {
      return check(
        "container mounts",
        Status::Fail,
        format!("cannot tell whether {name} is running: {error}"),
      );
    }
  };

  if !running.contains(&name) {
    return check(
      "container mounts",
      Status::Ok,
      format!("{name} is not running, so the next `compostbin run` mounts what the manifest says"),
    );
  }

  let record = match Record::load(&session.mount_record()) {
    Ok(Some(record)) => record,
    Ok(None) => {
      return check(
        "container mounts",
        Status::Warn,
        format!(
          "{name} is running but nothing recorded what it was started with; `compostbin stop` then `compostbin run` to be sure"
        ),
      );
    }
    Err(error) => return check("container mounts", Status::Warn, error.to_string()),
  };

  let drift = record.drift(&session.mounts());

  if drift.is_empty() {
    return check(
      "container mounts",
      Status::Ok,
      format!("{name} is running with the mounts the manifest declares"),
    );
  }

  check(
    "container mounts",
    Status::Warn,
    format!(
      "{name} was started before the manifest changed — {}; mounts cannot be added to a running container, so `compostbin stop` then `compostbin run`",
      drift.describe()
    ),
  )
}

/// Every mounted path, not only roots: a manifest can be edited by hand, so
/// `add`'s refusal is not the only way a dangerous path gets in.
fn root_breadth(session: &Session) -> Check {
  let dangerous: Vec<String> = session
    .workspace()
    .entries()
    .iter()
    .filter_map(|entry| danger(&entry.host, session.resolver()).map(|danger| (entry, danger)))
    .map(|(entry, danger)| match danger {
      Danger::Broad(_) => format!(
        "{} covers a whole account, so the container can read and rewrite all of it",
        entry.host.display()
      ),
      Danger::Sensitive(path) => format!(
        "{} is mounted, exposing the credentials in {}",
        entry.host.display(),
        path.display()
      ),
    })
    .collect();

  if dangerous.is_empty() {
    return check("mount breadth", Status::Ok, "no mount covers an account or a secret");
  }

  check("mount breadth", Status::Warn, dangerous.join("; "))
}

/// Every allowlisted command runs on the host with the user's own privileges
/// (D6), so this is where that stops being invisible.
fn host_allowlist(session: &Session) -> Check {
  if session.manifest.host.is_empty() {
    return check(
      "host commands",
      Status::Ok,
      "no [host.commands]: the guest has no path to the host",
    );
  }

  let listed: Vec<String> = session
    .manifest
    .host
    .commands
    .iter()
    .map(|(name, command)| {
      let widened = if command.arguments { " (+ guest arguments)" } else { "" };
      format!("{name} = {}{widened}", command.argv.join(" "))
    })
    .collect();

  let widened = session
    .manifest
    .host
    .commands
    .values()
    .any(|command| command.arguments);

  check(
    "host commands",
    if widened { Status::Warn } else { Status::Ok },
    format!("run on the host as you: {}", listed.join("; ")),
  )
}

fn credentials_check(session: &Session, credentials: &impl CredentialSource, api_key_present: bool) -> Check {
  let seeded = session.claude_home().join(CREDENTIALS_FILE_NAME);

  if seeded.exists() {
    return check("credentials", Status::Ok, seeded.display().to_string());
  }

  match credentials.read() {
    Ok(Some(_)) => check("credentials", Status::Ok, format!("{KEYCHAIN_SERVICE} in the Keychain")),
    Ok(None) | Err(_) if api_key_present => check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set"),
    Ok(None) => check(
      "credentials",
      Status::Fail,
      format!("no {KEYCHAIN_SERVICE} entry and no ANTHROPIC_API_KEY; the session cannot authenticate"),
    ),
    Err(error) => check("credentials", Status::Fail, error.to_string()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::credentials::CREDENTIALS_FILE_NAME;
  use crate::error::CredentialError;
  use crate::manifest::Manifest;
  use crate::paths::PathResolver;
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
      mounts
        .detail
        .contains(&base.join("not-there").display().to_string()),
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
      dangling.detail.contains("vendor") && dangling.detail.contains("elsewhere"),
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
      breadth.detail.contains(
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
      breadth.detail.contains(".ssh"),
      "detail should name the secret: {breadth:?}"
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
    assert!(allowlist.detail.contains("test = cargo nextest run"), "{allowlist:?}");
    assert!(
      allowlist.detail.contains("(+ guest arguments)"),
      "the widened command must be marked: {allowlist:?}"
    );
  }

  /// The silent failure dogfooding found: the session kept running with the
  /// mount set it was created with, and nothing anywhere said the manifest had
  /// moved on.
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
      mounts.detail.contains("vendor"),
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

  /// Nothing has been started, so there is no mount set to disagree with — and
  /// a fresh checkout must not be told to stop a container that does not exist.
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
