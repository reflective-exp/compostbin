use crate::error::{At, CredentialError};
use crate::hex;
use serde_json::{Map, Value};
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

pub const CREDENTIALS_FILE_NAME: &str = ".credentials.json";
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// The Claude login; other credentials, such as MCP tokens, stay in their store.
const LOGIN: &str = "claudeAiOauth";

const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// `Ok(None)` means no entry, which is not an error since the session may use an API key.
pub trait CredentialStore {
  fn read(&self) -> Result<Option<String>, CredentialError>;
  fn write(&self, secret: &str) -> Result<(), CredentialError>;
  fn name(&self) -> &str;
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

  fn name(&self) -> &str {
    KEYCHAIN_SERVICE
  }
}

pub struct CredentialsFile {
  path: PathBuf,
  name: String,
}

impl From<&Path> for CredentialsFile {
  fn from(claude_home: &Path) -> Self {
    let path = claude_home.join(CREDENTIALS_FILE_NAME);
    let name = path.display().to_string();
    Self { path, name }
  }
}

impl CredentialStore for CredentialsFile {
  fn name(&self) -> &str {
    &self.name
  }

  fn read(&self) -> Result<Option<String>, CredentialError> {
    match std::fs::read_to_string(&self.path) {
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
      read => Ok(Some(read.at(&self.path)?.trim().to_string())),
    }
  }

  /// Renamed into place, so the session never reads a partial token.
  fn write(&self, secret: &str) -> Result<(), CredentialError> {
    use std::os::unix::fs::OpenOptionsExt;

    let staged = self.path.with_extension("json.compostbin");

    if let Some(home) = self.path.parent() {
      std::fs::create_dir_all(home).at(home)?;
    }

    std::fs::File::options()
      .write(true)
      .create(true)
      .truncate(true)
      .mode(0o600)
      .open(&staged)
      .and_then(|mut file| file.write_all(secret.as_bytes()))
      .at(&staged)?;

    Ok(std::fs::rename(&staged, &self.path).at(&self.path)?)
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
  fn name(&self) -> &str {
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

/// Empty when absent or unparsable, as when caught mid-write.
fn credentials(store: &impl CredentialStore) -> Result<Map<String, Value>, CredentialError> {
  Ok(
    store
      .read()?
      .and_then(|secret| serde_json::from_str(&secret).ok())
      .unwrap_or_default(),
  )
}

fn expires_at(credentials: &Map<String, Value>) -> Option<u64> {
  credentials.get(LOGIN)?.get("expiresAt")?.as_u64()
}

pub fn has_login(store: &impl CredentialStore) -> Result<bool, CredentialError> {
  Ok(expires_at(&credentials(store)?).is_some())
}

/// Copies the login from `from` unless `to`'s expires no earlier, which keeps one session from
/// replacing a token another has since refreshed.
pub fn sync(from: &impl CredentialStore, to: &impl CredentialStore) -> Result<(), CredentialError> {
  let source = credentials(from)?;

  let Some(expires) = expires_at(&source) else {
    return Ok(());
  };

  let mut target = credentials(to)?;

  if expires_at(&target).is_some_and(|kept| kept >= expires) {
    return Ok(());
  }

  target.insert(LOGIN.to_string(), source[LOGIN].clone());
  to.write(&Value::Object(target).to_string())
}

/// Saves on starting too, catching a token left by a session killed before it could save, and
/// checks once more on stopping.
pub fn watch(claude_home: &Path, store: &impl CredentialStore, stop: &AtomicBool) {
  let file = CredentialsFile::from(claude_home);
  let modified = || -> Option<SystemTime> {
    std::fs::metadata(&file.path)
      .and_then(|meta| meta.modified())
      .ok()
  };
  let mut seen = None;

  loop {
    let stopping = stop.load(Ordering::Relaxed);
    let current = modified();

    if current != seen {
      seen = current;
      let _ = sync(&file, store);
    }

    if stopping {
      return;
    }

    std::thread::sleep(WATCH_INTERVAL);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

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

  fn file(home: &TempDir) -> CredentialsFile {
    CredentialsFile::from(claude_home(home).as_path())
  }

  fn keychain(config_dir: Option<&str>) -> Keychain {
    Keychain::from(config_dir.map(OsStr::new))
  }

  #[test]
  fn reads_the_default_item_without_a_config_dir() {
    assert_eq!(keychain(None).name(), KEYCHAIN_SERVICE);
    assert_eq!(keychain(Some("")).name(), KEYCHAIN_SERVICE);
  }

  #[test]
  fn suffixes_the_item_with_the_config_dir_hash() {
    // printf '%s' /Users/me/.claude-work | shasum -a 256
    assert_eq!(
      keychain(Some("/Users/me/.claude-work")).name(),
      "Claude Code-credentials-1e91dd84"
    );
  }

  #[test]
  fn hashes_the_config_dir_in_composed_form() {
    assert_eq!(
      keychain(Some("/Users/me/cafe\u{301}")).name(),
      keychain(Some("/Users/me/caf\u{e9}")).name()
    );
  }

  #[test]
  fn writes_the_login_into_a_missing_claude_home() {
    let home = TempDir::new().expect("temp dir");

    sync(&FakeStore::from(token(1000).as_str()), &file(&home)).expect("sync should succeed");

    assert_eq!(
      std::fs::read_to_string(credentials_path(&home)).expect("credentials should exist"),
      token(1000)
    );
  }

  #[test]
  fn writes_the_login_into_a_logged_out_session() {
    let home = TempDir::new().expect("temp dir");
    session_holds(&home, r#"{"mcpOAuth":{"server":"kept"}}"#);

    sync(&FakeStore::from(token(1000).as_str()), &file(&home)).expect("sync should succeed");

    let written = credentials(&file(&home)).expect("credentials should read");
    assert_eq!(expires_at(&written), Some(1000));
    assert_eq!(
      written["mcpOAuth"]["server"], "kept",
      "the session's other credentials stay"
    );
  }

  #[test]
  fn writes_the_token_readable_only_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().expect("temp dir");

    sync(&FakeStore::from(token(1000).as_str()), &file(&home)).expect("sync should succeed");

    let mode = std::fs::metadata(credentials_path(&home))
      .expect("credentials should exist")
      .permissions()
      .mode();
    assert_eq!(mode & 0o777, 0o600, "a token must not be world-readable");
  }

  #[test]
  fn copies_a_login_to_an_empty_store() {
    let store = FakeStore::default();

    sync(&FakeStore::from(token(2000).as_str()), &store).expect("sync should succeed");
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn copies_a_token_that_outlasts_the_stored_one() {
    let store = FakeStore::from(token(1000).as_str());

    sync(&FakeStore::from(token(2000).as_str()), &store).expect("sync should succeed");
    assert_eq!(store.kept(), Some(token(2000)));
  }

  #[test]
  fn copies_only_the_login() {
    let store = FakeStore::from(r#"{"mcpOAuth":{"server":"host"}}"#);
    let session = FakeStore::from(r#"{"claudeAiOauth":{"expiresAt":2000},"mcpOAuth":{"server":"session"}}"#);

    sync(&session, &store).expect("sync should succeed");

    let kept = credentials(&store).expect("credentials should read");
    assert_eq!(expires_at(&kept), Some(2000));
    assert_eq!(kept["mcpOAuth"]["server"], "host");
  }

  #[test]
  fn keeps_a_token_refreshed_elsewhere() {
    let store = FakeStore::from(token(2000).as_str());

    sync(&FakeStore::from(token(1000).as_str()), &store).expect("sync should succeed");
    assert_eq!(store.writes(), 0);
  }

  #[test]
  fn copies_nothing_without_a_readable_login() {
    let home = TempDir::new().expect("temp dir");
    let store = FakeStore::default();

    sync(&file(&home), &store).expect("a missing file is not an error");

    session_holds(&home, r#"{"claudeAiOauth":{"accessTo"#);
    sync(&file(&home), &store).expect("a partial write is not an error");

    session_holds(&home, r#"{"mcpOAuth":{}}"#);
    sync(&file(&home), &store).expect("a logged-out session is not an error");
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
