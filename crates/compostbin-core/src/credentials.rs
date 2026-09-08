use crate::error::{CredentialError, PathError};
use std::path::Path;
use std::process::Command;

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
}
