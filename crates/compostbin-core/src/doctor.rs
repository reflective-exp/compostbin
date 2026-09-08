use crate::credentials::{CREDENTIALS_FILE_NAME, CredentialSource, KEYCHAIN_SERVICE};
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::error::EngineError;

/// The `container` CLI version every fact in the plan was measured against.
pub const TESTED_CLI_VERSION: &str = "1.3.1";
/// Roots so broad that mounting them hands the container the whole account.
pub const BROAD_ROOTS: [&str; 5] = ["/", "~", "~/Desktop", "~/Documents", "~/Downloads"];

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
    root_breadth(session),
    credentials_check(session, credentials, api_key_present),
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

fn root_breadth(session: &Session) -> Check {
  let broad: Vec<String> = session
    .manifest
    .workspace
    .roots
    .iter()
    .map(|raw| session.resolve(raw))
    .filter(|root| {
      BROAD_ROOTS
        .iter()
        .any(|broad| session.resolve(broad) == *root)
    })
    .map(|root| root.display().to_string())
    .collect();

  if broad.is_empty() {
    return check("root breadth", Status::Ok, "no root covers a whole account");
  }

  check(
    "root breadth",
    Status::Warn,
    format!(
      "{} is mounted writable, so anything in the container can rewrite it",
      broad.join(", ")
    ),
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
  fn warns_about_a_root_covering_the_whole_home_directory() {
    let home = TempDir::new().expect("temp dir");

    let checks = diagnose(
      &session(&home, "\"~\""),
      &RecordingEngine::with_images(&["compostbin/base:latest"]),
      &in_keychain(),
      false,
    );

    let breadth = check(&checks, "root breadth");
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
}
