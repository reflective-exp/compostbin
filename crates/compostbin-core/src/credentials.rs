use crate::error::{CredentialError, PathError};
use std::path::Path;
use std::process::Command;

/// The host user's own Claude home, the source of the shared config copied into
/// each session.
pub const HOST_CLAUDE_HOME: &str = "~/.claude";
/// Where Claude looks for its OAuth token inside its home directory.
pub const CREDENTIALS_FILE_NAME: &str = ".credentials.json";
/// The Keychain item the host's Claude Code writes its token to.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// What `seed` did, so the caller can say so without re-reading the filesystem.
#[derive(Clone, Debug, PartialEq)]
pub enum SeedOutcome {
  Disabled,
  KeptExisting,
  NotInKeychain,
  Seeded,
}

/// A place to read the host's Claude token from. `Ok(None)` means "no such
/// entry", which is not an error: the session may authenticate by API key.
pub trait CredentialSource {
  fn read(&self) -> Result<Option<String>, CredentialError>;
}

pub struct Keychain;

/// `security` exits 44 when the item is simply absent, which is not a failure.
const KEYCHAIN_ITEM_NOT_FOUND: i32 = 44;

impl CredentialSource for Keychain {
  fn read(&self) -> Result<Option<String>, CredentialError> {
    let output = Command::new("security")
      .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
      .output()
      .map_err(|source| CredentialError::Io(PathError::new("security", source)))?;

    if output.status.code() == Some(KEYCHAIN_ITEM_NOT_FOUND) {
      return Ok(None);
    }

    if !output.status.success() {
      return Err(CredentialError::Keychain(
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
      ));
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).trim().to_string()))
  }
}

/// Copies the host's Claude token into the bind-mounted Claude home, but only
/// when there is no token there already: the container refreshes its own token
/// into that same file, and it may be newer than the Keychain's.
pub fn seed(claude_home: &Path, enabled: bool, source: &impl CredentialSource) -> Result<SeedOutcome, CredentialError> {
  if !enabled {
    return Ok(SeedOutcome::Disabled);
  }

  let destination = claude_home.join(CREDENTIALS_FILE_NAME);

  if destination.exists() {
    return Ok(SeedOutcome::KeptExisting);
  }

  let Some(secret) = source.read()? else {
    return Ok(SeedOutcome::NotInKeychain);
  };

  std::fs::create_dir_all(claude_home).map_err(|source| CredentialError::Io(PathError::new(claude_home, source)))?;
  std::fs::write(&destination, secret).map_err(|source| CredentialError::Io(PathError::new(&destination, source)))?;
  restrict_to_owner(&destination)?;

  Ok(SeedOutcome::Seeded)
}

/// Copies the named files from the host's own `~/.claude` into the session's
/// home, overwriting what is there.
///
/// The host is authoritative on purpose: these are the files the user maintains
/// once and expects in every session, so an edit on the host must reach the next
/// session rather than being shadowed by a stale copy. Everything else in the
/// session home — history, projects, the token the container refreshes — is
/// session state and is never overwritten from here. Names are joined as single
/// components, so a manifest cannot reach outside `~/.claude` with `../`.
pub fn share(host_home: &Path, session_home: &Path, names: &[String]) -> Result<Vec<String>, CredentialError> {
  let mut copied = Vec::new();

  for name in names {
    let source = host_home.join(name);
    if name.contains('/') || !source.is_file() {
      continue;
    }

    std::fs::create_dir_all(session_home)
      .map_err(|source| CredentialError::Io(PathError::new(session_home, source)))?;
    let destination = session_home.join(name);
    std::fs::copy(&source, &destination).map_err(|error| CredentialError::Io(PathError::new(&destination, error)))?;
    copied.push(name.clone());
  }

  Ok(copied)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<(), CredentialError> {
  use std::os::unix::fs::PermissionsExt;

  std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    .map_err(|source| CredentialError::Io(PathError::new(path, source)))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<(), CredentialError> {
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;
  use tempfile::TempDir;

  const SECRET: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-test"}}"#;

  struct FakeSource(Option<String>);

  impl CredentialSource for FakeSource {
    fn read(&self) -> Result<Option<String>, CredentialError> {
      Ok(self.0.clone())
    }
  }

  fn found() -> FakeSource {
    FakeSource(Some(SECRET.to_string()))
  }

  fn credentials_path(home: &TempDir) -> PathBuf {
    home.path().join("claude-home").join(CREDENTIALS_FILE_NAME)
  }

  fn claude_home(home: &TempDir) -> PathBuf {
    home.path().join("claude-home")
  }

  #[test]
  fn writes_the_keychain_secret_into_a_missing_claude_home() {
    let home = TempDir::new().expect("temp dir");

    let outcome = seed(&claude_home(&home), true, &found()).expect("seeding should succeed");

    assert_eq!(outcome, SeedOutcome::Seeded);
    assert_eq!(
      std::fs::read_to_string(credentials_path(&home)).expect("credentials should exist"),
      SECRET
    );
  }

  #[test]
  fn keeps_a_token_the_container_already_refreshed() {
    let home = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(claude_home(&home)).expect("create home");
    std::fs::write(credentials_path(&home), "newer").expect("write existing");

    let outcome = seed(&claude_home(&home), true, &found()).expect("seeding should succeed");

    assert_eq!(outcome, SeedOutcome::KeptExisting);
    assert_eq!(
      std::fs::read_to_string(credentials_path(&home)).expect("credentials should exist"),
      "newer"
    );
  }

  #[test]
  fn writes_nothing_when_seeding_is_disabled() {
    let home = TempDir::new().expect("temp dir");

    let outcome = seed(&claude_home(&home), false, &found()).expect("seeding should succeed");

    assert_eq!(outcome, SeedOutcome::Disabled);
    assert!(!credentials_path(&home).exists());
  }

  #[test]
  fn reports_a_missing_keychain_entry_without_failing() {
    let home = TempDir::new().expect("temp dir");

    let outcome = seed(&claude_home(&home), true, &FakeSource(None)).expect("seeding should succeed");

    assert_eq!(outcome, SeedOutcome::NotInKeychain);
    assert!(!credentials_path(&home).exists());
  }

  #[test]
  #[cfg(unix)]
  fn writes_the_token_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().expect("temp dir");

    seed(&claude_home(&home), true, &found()).expect("seeding should succeed");

    let mode = std::fs::metadata(credentials_path(&home))
      .expect("credentials should exist")
      .permissions()
      .mode();
    assert_eq!(mode & 0o777, 0o600, "a token must not be world-readable");
  }

  #[test]
  fn copies_shared_config_from_the_host() {
    let temp = TempDir::new().expect("temp dir");
    let host = temp.path().join("host-claude");
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&host).expect("create host home");
    std::fs::write(host.join("CLAUDE.md"), "house style").expect("write CLAUDE.md");
    std::fs::write(host.join("settings.json"), "{}").expect("write settings");
    std::fs::write(host.join("history.jsonl"), "not shared").expect("write history");

    let copied =
      share(&host, &session, &["CLAUDE.md".to_string(), "settings.json".to_string()]).expect("share should succeed");

    assert_eq!(copied, ["CLAUDE.md", "settings.json"]);
    assert_eq!(
      std::fs::read_to_string(session.join("CLAUDE.md")).expect("CLAUDE.md should be copied"),
      "house style"
    );
    assert!(
      !session.join("history.jsonl").exists(),
      "only the named files are shared"
    );
  }

  #[test]
  fn shared_config_follows_the_host_on_every_run() {
    let temp = TempDir::new().expect("temp dir");
    let host = temp.path().join("host-claude");
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&host).expect("create host home");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::write(session.join("CLAUDE.md"), "stale").expect("write stale copy");
    std::fs::write(host.join("CLAUDE.md"), "edited on the host").expect("write CLAUDE.md");

    share(&host, &session, &["CLAUDE.md".to_string()]).expect("share should succeed");

    assert_eq!(
      std::fs::read_to_string(session.join("CLAUDE.md")).expect("CLAUDE.md should exist"),
      "edited on the host"
    );
  }

  #[test]
  fn shares_nothing_the_host_does_not_have() {
    let temp = TempDir::new().expect("temp dir");
    let host = temp.path().join("host-claude");
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&host).expect("create host home");

    let copied = share(
      &host,
      &session,
      &["CLAUDE.md".to_string(), "../.ssh/id_ed25519".to_string()],
    )
    .expect("share should succeed");

    assert_eq!(copied, Vec::<String>::new());
    assert!(!session.exists(), "nothing to copy means nothing to create");
  }
}
