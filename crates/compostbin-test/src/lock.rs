use crate::signing::target_dir;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(50);

/// A file rather than a `Mutex`, because nextest runs each test in its own
/// process.
pub fn exclusive(what: &str) -> Lock {
  Lock::take(target_dir().join(format!("compostbin-test-{what}.lock")))
}

pub struct Lock(PathBuf);

impl Lock {
  pub(crate) fn take(path: PathBuf) -> Self {
    loop {
      match std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
      {
        Ok(_) => return Self(path),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
          if held_too_long(&path) {
            let _ = std::fs::remove_file(&path);
          }
          std::thread::sleep(LOCK_POLL);
        }
        Err(error) => panic!("could not take {}: {error}", path.display()),
      }
    }
  }
}

fn held_too_long(path: &Path) -> bool {
  std::fs::metadata(path)
    .and_then(|metadata| metadata.modified())
    .is_ok_and(|taken| taken.elapsed().is_ok_and(|held| held > LOCK_TIMEOUT))
}

impl Drop for Lock {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}
