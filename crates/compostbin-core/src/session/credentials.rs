use crate::error::{At, CredentialError};
use crate::hex;
use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

pub const CREDENTIALS_FILE_NAME: &str = ".credentials.json";
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// `Ok(None)` means no entry, which is not an error since the session may use an API key.
pub trait CredentialStore {
  fn read(&self) -> Result<Option<String>, CredentialError>;
  fn write(&self, secret: &str) -> Result<(), CredentialError>;
  fn service(&self) -> &str;
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeStore {
  kept: std::sync::Mutex<Option<String>>,
  writes: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl From<&str> for FakeStore {
  fn from(kept: &str) -> Self {
    Self {
      kept: std::sync::Mutex::new(Some(kept.to_string())),
      ..Self::default()
    }
  }
}

#[cfg(test)]
impl FakeStore {
  pub(crate) fn kept(&self) -> Option<String> {
    self.kept.lock().expect("store lock").clone()
  }

  pub(crate) fn writes(&self) -> usize {
    self.writes.load(Ordering::Relaxed)
  }
}

#[cfg(test)]
impl CredentialStore for FakeStore {
  fn read(&self) -> Result<Option<String>, CredentialError> {
    Ok(self.kept())
  }

  fn write(&self, secret: &str) -> Result<(), CredentialError> {
    *self.kept.lock().expect("store lock") = Some(secret.to_string());
    self.writes.fetch_add(1, Ordering::Relaxed);
    Ok(())
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
        let suffix = hex::encode(&Sha256::digest(normalized.as_bytes())[..4]);
        format!("{KEYCHAIN_SERVICE}-{suffix}")
      }
    };

    Self { service }
  }
}

/// `security` exits 44 when the item is absent.
const KEYCHAIN_ITEM_NOT_FOUND: i32 = 44;

impl CredentialStore for Keychain {
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

  /// Sent on stdin, hex-encoded, so the token never appears in the process list. The account is
  /// the login name, as Claude Code writes it, so `-U` updates its item rather than adding one.
  fn write(&self, secret: &str) -> Result<(), CredentialError> {
    let account = std::env::var("USER").map_err(|_| CredentialError::KeychainSave("USER is not set".to_string()))?;
    let hex = hex::encode(secret.as_bytes());

    let mut child = Command::new("security")
      .arg("-i")
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .spawn()
      .map_err(CredentialError::Security)?;

    if let Some(mut stdin) = child.stdin.take() {
      writeln!(
        stdin,
        "add-generic-password -U -a \"{account}\" -s \"{}\" -X {hex}",
        self.service
      )
      .map_err(CredentialError::Security)?;
    }

    let output = child
      .wait_with_output()
      .map_err(CredentialError::Security)?;
    let errors = String::from_utf8_lossy(&output.stderr).trim().to_string();

    // `security -i` can exit 0 after a failed command, so anything on stderr is a failure too.
    if !output.status.success() || !errors.is_empty() {
      return Err(CredentialError::KeychainSave(errors));
    }

    Ok(())
  }
}

fn expires_at(secret: &str) -> Option<u64> {
  serde_json::from_str::<serde_json::Value>(secret)
    .ok()?
    .get("claudeAiOauth")?
    .get("expiresAt")?
    .as_u64()
}

/// Copies the session's token to the Keychain unless the Keychain's expires no earlier, which
/// keeps one session from replacing a token another has since refreshed.
pub fn save(claude_home: &Path, store: &impl CredentialStore) -> Result<(), CredentialError> {
  let path = claude_home.join(CREDENTIALS_FILE_NAME);

  let secret = match std::fs::read_to_string(&path) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    read => read.at(&path)?,
  };
  let secret = secret.trim();

  // Unparsable when caught mid-write; the finished write changes the mtime again.
  let Some(expires) = expires_at(secret) else {
    return Ok(());
  };

  if store
    .read()?
    .as_deref()
    .and_then(expires_at)
    .is_some_and(|kept| kept >= expires)
  {
    return Ok(());
  }

  store.write(secret)
}

/// Saves on starting too, catching a token left by a session killed before it could save, and
/// checks once more on stopping.
pub fn watch(claude_home: &Path, store: &impl CredentialStore, stop: &AtomicBool) {
  let path = claude_home.join(CREDENTIALS_FILE_NAME);
  let modified = || -> Option<SystemTime> {
    std::fs::metadata(&path)
      .and_then(|meta| meta.modified())
      .ok()
  };
  let mut seen = None;

  loop {
    let stopping = stop.load(Ordering::Relaxed);
    let current = modified();

    if current != seen {
      seen = current;
      let _ = save(claude_home, store);
    }

    if stopping {
      return;
    }

    std::thread::sleep(WATCH_INTERVAL);
  }
}

/// Never overwrites an existing token, which the container may have refreshed past the Keychain's.
/// Returns true when a token was wanted and the Keychain had none.
pub fn seed(claude_home: &Path, store: &impl CredentialStore) -> Result<bool, CredentialError> {
  use std::os::unix::fs::OpenOptionsExt;

  let destination = claude_home.join(CREDENTIALS_FILE_NAME);

  if destination.exists() {
    return Ok(false);
  }

  let Some(secret) = store.read()? else {
    return Ok(true);
  };

  std::fs::create_dir_all(claude_home).at(claude_home)?;
  std::fs::File::options()
    .write(true)
    .create_new(true)
    .mode(0o600)
    .open(&destination)
    .and_then(|mut file| file.write_all(secret.as_bytes()))
    .at(&destination)?;

  Ok(false)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;
  use tempfile::TempDir;

  const SECRET: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-test"}}"#;

  fn token(expires_at: u64) -> String {
    format!(r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat-{expires_at}","expiresAt":{expires_at}}}}}"#)
  }

  fn session_holds(home: &TempDir, secret: &str) {
    std::fs::create_dir_all(claude_home(home)).expect("create home");
    std::fs::write(credentials_path(home), secret).expect("write credentials");
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

    let missing = seed(&claude_home(&home), &FakeStore::from(SECRET)).expect("seeding should succeed");

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

    let missing = seed(&claude_home(&home), &FakeStore::default()).expect("seeding should succeed");

    assert!(!missing, "a token already there needs nothing from the Keychain");
    assert_eq!(
      std::fs::read_to_string(credentials_path(&home)).expect("credentials should exist"),
      "newer"
    );
  }

  #[test]
  fn reports_a_missing_keychain_entry_without_failing() {
    let home = TempDir::new().expect("temp dir");

    let missing = seed(&claude_home(&home), &FakeStore::default()).expect("seeding should succeed");

    assert!(missing);
    assert!(!credentials_path(&home).exists());
  }

  #[test]
  fn writes_the_token_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().expect("temp dir");

    seed(&claude_home(&home), &FakeStore::from(SECRET)).expect("seeding should succeed");

    let mode = std::fs::metadata(credentials_path(&home))
      .expect("credentials should exist")
      .permissions()
      .mode();
    assert_eq!(mode & 0o777, 0o600, "a token must not be world-readable");
  }

  #[test]
  fn saves_a_login_to_an_empty_keychain() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = FakeStore::default();

    save(&claude_home(&home), &store).expect("save should succeed");
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn saves_a_token_that_outlasts_the_keychains() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = FakeStore::from(token(1000).as_str());

    save(&claude_home(&home), &store).expect("save should succeed");
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn keeps_a_keychain_token_refreshed_elsewhere() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(1000));
    let store = FakeStore::from(token(2000).as_str());

    save(&claude_home(&home), &store).expect("save should succeed");
    assert_eq!(store.writes(), 0);
  }

  #[test]
  fn saves_nothing_without_a_readable_token() {
    let home = TempDir::new().expect("temp dir");
    let store = FakeStore::default();

    save(&claude_home(&home), &store).expect("a missing file is not an error");

    session_holds(&home, r#"{"claudeAiOauth":{"accessTo"#);
    save(&claude_home(&home), &store).expect("a partial write is not an error");
    assert_eq!(store.writes(), 0);
  }

  #[test]
  fn watching_saves_a_token_left_before_it_started() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = FakeStore::from(token(1000).as_str());

    watch(&claude_home(&home), &store, &AtomicBool::new(true));

    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn watching_saves_a_token_written_while_it_runs() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(1000));
    let store = FakeStore::default();
    let stop = AtomicBool::new(false);

    std::thread::scope(|scope| {
      scope.spawn(|| watch(&claude_home(&home), &store, &stop));

      while store.writes() == 0 {
        std::thread::sleep(Duration::from_millis(10));
      }

      session_holds(&home, &token(2000));
      stop.store(true, Ordering::Relaxed);
    });

    assert_eq!(store.kept(), Some(token(2000)), "stopping should catch the last write");
  }
}
