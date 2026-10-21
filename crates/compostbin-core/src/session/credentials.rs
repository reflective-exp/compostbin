use crate::error::{At, CredentialError};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

pub const CREDENTIALS_FILE_NAME: &str = ".credentials.json";
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// `Ok(None)` means no entry, which is not an error since the session may use an API key.
pub trait CredentialSource {
  fn read(&self) -> Result<Option<String>, CredentialError>;
  fn service(&self) -> &str;
}

#[cfg(test)]
pub(crate) struct FakeSource(pub Option<String>);

#[cfg(test)]
impl CredentialSource for FakeSource {
  fn read(&self) -> Result<Option<String>, CredentialError> {
    Ok(self.0.clone())
  }

  fn service(&self) -> &str {
    KEYCHAIN_SERVICE
  }
}

pub struct Keychain {
  service: String,
}

/// Claude Code names the item after the host's `CLAUDE_CONFIG_DIR` whenever it is set, even to
/// the default, hashing the value as written: `~/.claude` and its realpath name different items.
impl From<Option<&OsStr>> for Keychain {
  fn from(config_dir: Option<&OsStr>) -> Self {
    use sha2::{Digest, Sha256};
    use unicode_normalization::UnicodeNormalization;

    let service = match config_dir.filter(|dir| !dir.is_empty()) {
      None => KEYCHAIN_SERVICE.to_string(),
      Some(dir) => {
        let normalized: String = dir.to_string_lossy().nfc().collect();
        let suffix: String = Sha256::digest(normalized.as_bytes())[..4]
          .iter()
          .map(|byte| format!("{byte:02x}"))
          .collect();
        format!("{KEYCHAIN_SERVICE}-{suffix}")
      }
    };

    Self { service }
  }
}

/// `security` exits 44 when the item is absent.
const KEYCHAIN_ITEM_NOT_FOUND: i32 = 44;

impl CredentialSource for Keychain {
  fn service(&self) -> &str {
    &self.service
  }

  fn read(&self) -> Result<Option<String>, CredentialError> {
    let output = Command::new("security")
      .args(["find-generic-password", "-s", &self.service, "-w"])
      .output()
      .map_err(CredentialError::Security)?;

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

/// Never overwrites an existing token, which the container may have refreshed past the Keychain's.
/// Returns true when a token was wanted and the Keychain had none.
pub fn seed(claude_home: &Path, enabled: bool, source: &impl CredentialSource) -> Result<bool, CredentialError> {
  let destination = claude_home.join(CREDENTIALS_FILE_NAME);

  if !enabled || destination.exists() {
    return Ok(false);
  }

  let Some(secret) = source.read()? else {
    return Ok(true);
  };

  std::fs::create_dir_all(claude_home).at(claude_home)?;
  std::fs::write(&destination, secret).at(&destination)?;
  restrict_to_owner(&destination)?;

  Ok(false)
}

fn restrict_to_owner(path: &Path) -> Result<(), CredentialError> {
  use std::os::unix::fs::PermissionsExt;

  Ok(std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).at(path)?)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;
  use tempfile::TempDir;

  const SECRET: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-test"}}"#;

  fn found() -> FakeSource {
    FakeSource(Some(SECRET.to_string()))
  }

  fn claude_home(home: &TempDir) -> PathBuf {
    home.path().join("claude-home")
  }

  fn credentials_path(home: &TempDir) -> PathBuf {
    claude_home(home).join(CREDENTIALS_FILE_NAME)
  }

  fn keychain(config_dir: Option<&str>) -> Keychain {
    Keychain::from(config_dir.map(OsStr::new))
  }

  #[test]
  fn reads_the_default_item_without_a_config_dir() {
    assert_eq!(keychain(None).service(), KEYCHAIN_SERVICE);
    assert_eq!(keychain(Some("")).service(), KEYCHAIN_SERVICE);
  }

  #[test]
  fn suffixes_the_item_with_the_config_dir_hash() {
    // printf '%s' /Users/me/.claude-work | shasum -a 256
    assert_eq!(
      keychain(Some("/Users/me/.claude-work")).service(),
      "Claude Code-credentials-1e91dd84"
    );
  }

  #[test]
  fn hashes_the_config_dir_in_composed_form() {
    assert_eq!(
      keychain(Some("/Users/me/cafe\u{301}")).service(),
      keychain(Some("/Users/me/caf\u{e9}")).service()
    );
  }

  #[test]
  fn writes_the_keychain_secret_into_a_missing_claude_home() {
    let home = TempDir::new().expect("temp dir");

    let missing = seed(&claude_home(&home), true, &found()).expect("seeding should succeed");

    assert!(!missing);
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

    let missing = seed(&claude_home(&home), true, &FakeSource(None)).expect("seeding should succeed");

    assert!(!missing, "a token already there needs nothing from the Keychain");
    assert_eq!(
      std::fs::read_to_string(credentials_path(&home)).expect("credentials should exist"),
      "newer"
    );
  }

  #[test]
  fn writes_nothing_when_seeding_is_disabled() {
    let home = TempDir::new().expect("temp dir");

    let missing = seed(&claude_home(&home), false, &FakeSource(None)).expect("seeding should succeed");

    assert!(!missing, "a disabled seed never asks the Keychain");
    assert!(!credentials_path(&home).exists());
  }

  #[test]
  fn reports_a_missing_keychain_entry_without_failing() {
    let home = TempDir::new().expect("temp dir");

    let missing = seed(&claude_home(&home), true, &FakeSource(None)).expect("seeding should succeed");

    assert!(missing);
    assert!(!credentials_path(&home).exists());
  }

  #[test]
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
