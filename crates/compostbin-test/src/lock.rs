//! A lock across test processes, for what the whole machine shares.

use crate::signing::target_dir;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Long enough for a copy and a `codesign`, short enough that a lock left by a
/// killed test does not stop the next run.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_POLL: Duration = Duration::from_millis(50);

/// Serializes the tests that touch one thing the whole machine shares — the
/// pasteboard, above all. A `Mutex` would not: nextest runs each test in a
/// process of its own, so the lock has to be one too.
pub fn exclusive(what: &str) -> Lock {
  Lock::take(target_dir().join(format!("compostbin-test-{what}.lock")))
}

/// A lock held by the existence of a file, released by dropping it. One left by
/// a test that died is taken over once it is older than `LOCK_TIMEOUT`, so a
/// crash costs one slow run rather than every run after it.
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

/// Whether a lock file is old enough that whoever made it is gone. A missing
/// one has just been released, which is not a timeout.
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
