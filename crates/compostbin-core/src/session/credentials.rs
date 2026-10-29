use crate::error::{At, CredentialError};
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
pub trait CredentialSource {
  fn read(&self) -> Result<Option<String>, CredentialError>;
  fn write(&self, secret: &str) -> Result<(), CredentialError>;
  fn service(&self) -> &str;
}

#[cfg(test)]
pub(crate) struct FakeSource(pub Option<String>);

#[cfg(test)]
impl CredentialSource for FakeSource {
  fn read(&self) -> Result<Option<String>, CredentialError> {
    Ok(self.0.clone())
  }

  fn write(&self, _secret: &str) -> Result<(), CredentialError> {
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

  /// Sent on stdin, hex-encoded, so the token never appears in the process list. The account is
  /// the login name, as Claude Code writes it, so `-U` updates its item rather than adding one.
  fn write(&self, secret: &str) -> Result<(), CredentialError> {
    let account = std::env::var("USER").map_err(|_| CredentialError::KeychainSave("USER is not set".to_string()))?;
    let hex: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();

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
/// keeps one session from replacing a token another has since refreshed. Returns whether it wrote.
pub fn save(claude_home: &Path, store: &impl CredentialSource) -> Result<bool, CredentialError> {
  let path = claude_home.join(CREDENTIALS_FILE_NAME);

  let secret = match std::fs::read_to_string(&path) {
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
    read => read.at(&path)?,
  };
  let secret = secret.trim();

  // Unparsable when caught mid-write; the finished write changes the mtime again.
  let Some(expires) = expires_at(secret) else {
    return Ok(false);
  };

  if store
    .read()?
    .as_deref()
    .and_then(expires_at)
    .is_some_and(|kept| kept >= expires)
  {
    return Ok(false);
  }

  store.write(secret)?;
  Ok(true)
}

/// Saves on starting too, catching a token left by a session killed before it could save, and
/// checks once more on stopping.
pub fn watch(claude_home: &Path, store: &impl CredentialSource, stop: &AtomicBool) {
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
  use std::sync::Mutex;
  use std::sync::atomic::AtomicUsize;
  use tempfile::TempDir;

  const SECRET: &str = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-test"}}"#;

  fn found() -> FakeSource {
    FakeSource(Some(SECRET.to_string()))
  }

  fn token(expires_at: u64) -> String {
    format!(r#"{{"claudeAiOauth":{{"accessToken":"sk-ant-oat-{expires_at}","expiresAt":{expires_at}}}}}"#)
  }

  struct Store {
    kept: Mutex<Option<String>>,
    writes: AtomicUsize,
  }

  impl Store {
    fn holding(kept: Option<String>) -> Self {
      Self {
        kept: Mutex::new(kept),
        writes: AtomicUsize::new(0),
      }
    }

    fn kept(&self) -> Option<String> {
      self.kept.lock().expect("store lock").clone()
    }

    fn writes(&self) -> usize {
      self.writes.load(Ordering::Relaxed)
    }
  }

  impl CredentialSource for Store {
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

  #[test]
  fn saves_a_login_to_an_empty_keychain() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = Store::holding(None);

    assert!(save(&claude_home(&home), &store).expect("save should succeed"));
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn saves_a_token_that_outlasts_the_keychains() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = Store::holding(Some(token(1000)));

    assert!(save(&claude_home(&home), &store).expect("save should succeed"));
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn keeps_a_keychain_token_refreshed_elsewhere() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(1000));
    let store = Store::holding(Some(token(2000)));

    assert!(!save(&claude_home(&home), &store).expect("save should succeed"));
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn saves_nothing_without_a_readable_token() {
    let home = TempDir::new().expect("temp dir");
    let store = Store::holding(None);

    assert!(!save(&claude_home(&home), &store).expect("a missing file is not an error"));

    session_holds(&home, r#"{"claudeAiOauth":{"accessTo"#);
    assert!(!save(&claude_home(&home), &store).expect("a partial write is not an error"));
    assert_eq!(store.writes(), 0);
  }

  #[test]
  fn watching_saves_a_token_left_before_it_started() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(2000));
    let store = Store::holding(Some(token(1000)));

    watch(&claude_home(&home), &store, &AtomicBool::new(true));

    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn watching_saves_a_token_written_while_it_runs() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, &token(1000));
    let store = Store::holding(None);
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
