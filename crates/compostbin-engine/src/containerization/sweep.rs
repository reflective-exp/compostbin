use super::snapshots::Snapshots;
use super::store::Store;
use super::unpacked::Unpacked;
use super::{control, control_socket, oci};
use crate::error::EngineError;
use containerization_framework::containerization::image::{Image, ImageStore};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Locked for life by the process that created a directory in `containers`.
const OWNER_LOCK: &str = ".owner";
/// Holds the pid of a build from an earlier version.
const LEGACY_BUILDER_MARKER: &str = ".builder";
/// Shared by builds until they tag their image; exclusive while blobs are reclaimed.
const RECLAIM_LOCK: &str = "reclaim.lock";
/// Covers the gap between creating a directory and claiming it.
const CLAIM_GRACE: Duration = Duration::from_secs(60);

/// Released on drop or process exit.
#[must_use = "the lock is released when this is dropped"]
pub struct Lock {
  _file: File,
}

fn open(path: &Path) -> std::io::Result<File> {
  File::options()
    .create(true)
    .truncate(false)
    .write(true)
    .open(path)
}

pub fn claim(directory: &Path) -> std::io::Result<Lock> {
  let file = open(&directory.join(OWNER_LOCK))?;
  file.lock()?;

  Ok(Lock { _file: file })
}

/// Blocks while blobs are being reclaimed.
pub fn building(store: &Store) -> std::io::Result<Lock> {
  let file = open(&store.root().join(RECLAIM_LOCK))?;
  file.lock_shared()?;

  Ok(Lock { _file: file })
}

/// An unclaimed directory younger than [`CLAIM_GRACE`] counts as alive; `None` for an older one.
fn owner_lives(directory: &Path) -> Option<bool> {
  if let Ok(file) = File::open(directory.join(OWNER_LOCK)) {
    return Some(matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock)));
  }

  if let Some(pid) = std::fs::read_to_string(directory.join(LEGACY_BUILDER_MARKER))
    .ok()
    .and_then(|owner| owner.trim().parse::<libc::pid_t>().ok())
  {
    // SAFETY: signal 0 only checks whether the pid exists; EPERM means it does, under another user.
    return Some(
      unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM),
    );
  }

  let young = std::fs::metadata(directory)
    .and_then(|metadata| metadata.modified())
    .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age < CLAIM_GRACE));

  young.then_some(true)
}

/// Removes container directories whose owner has gone. With `runtime_dir`, also unclaimed ones whose control socket there does not answer.
pub fn abandoned(store: &Store, runtime_dir: Option<&Path>) -> Vec<PathBuf> {
  std::fs::read_dir(store.containers())
    .into_iter()
    .flatten()
    .flatten()
    .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.path()))
    .filter(|(name, directory)| {
      directory.is_dir()
        && match owner_lives(directory) {
          Some(lives) => !lives,
          None => runtime_dir.is_some_and(|runtime_dir| !control::served(&control_socket(runtime_dir, name))),
        }
    })
    .map(|(_, directory)| directory)
    .filter(|directory| std::fs::remove_dir_all(directory).is_ok())
    .collect()
}

#[derive(Debug, Eq, PartialEq)]
pub enum Blobs {
  /// A build's blobs are unreferenced until it tags its image.
  BuildRunning,
  Reclaimed {
    count: usize,
    freed_in_bytes: u64,
  },
}

pub fn describe(reclaimed: &Result<Blobs, EngineError>) -> Option<String> {
  match reclaimed {
    Ok(Blobs::BuildRunning) => Some("left unreferenced blobs alone while a build runs".to_string()),
    Ok(Blobs::Reclaimed { count: 0, .. }) => None,
    Ok(Blobs::Reclaimed { count, freed_in_bytes }) => Some(format!(
      "reclaimed {} MB from {count} unreferenced blob{}",
      freed_in_bytes >> 20,
      if *count == 1 { "" } else { "s" }
    )),
    Err(error) => Some(format!("could not reclaim unreferenced blobs: {error}")),
  }
}

/// Evicts unpacked images no tag refers to, then unreferenced blobs unless a build holds [`building`].
pub fn reclaim(store: &Store, images: &ImageStore) -> (Vec<PathBuf>, Result<Blobs, EngineError>) {
  let evicted = images
    .list()
    .map(|listed| Unpacked::at(store.unpacked()).evict(listed.iter().map(Image::digest)))
    .unwrap_or_default();

  let blobs = (|| {
    let file =
      open(&store.root().join(RECLAIM_LOCK)).map_err(|error| EngineError::failed("lock the image store", error))?;

    match file.try_lock() {
      Ok(()) => {}
      Err(TryLockError::WouldBlock) => return Ok(Blobs::BuildRunning),
      Err(TryLockError::Error(error)) => return Err(EngineError::failed("lock the image store", error)),
    }

    let (deleted, freed_in_bytes) = images.clean_up_orphaned_blobs()?;

    Ok(Blobs::Reclaimed {
      count: deleted.len(),
      freed_in_bytes,
    })
  })();

  (evicted, blobs)
}

#[derive(Debug)]
pub struct Swept {
  pub removed: Vec<PathBuf>,
  /// `None` without a store.
  pub blobs: Option<Result<Blobs, EngineError>>,
}

/// Container directories are swept even when the store cannot be read.
pub fn sweep(store: &Store, runtime_dir: &Path) -> Swept {
  if !store.root().is_dir() {
    return Swept {
      removed: Vec::new(),
      blobs: None,
    };
  }

  let mut removed = abandoned(store, Some(runtime_dir));

  let blobs = store.content().and_then(|(content, images)| {
    let (evicted, blobs) = reclaim(store, &images);
    removed.extend(evicted);
    removed.extend(Snapshots::at(store.build_cache()).forget_images(|descriptor| !oci::holds(&content, descriptor)));

    blobs
  });

  Swept {
    removed,
    blobs: Some(blobs),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn directory(store: &Store, name: &str) -> PathBuf {
    let path = store.container_dir(name);
    std::fs::create_dir_all(&path).expect("a container directory");
    path
  }

  fn age(path: &Path) {
    File::open(path)
      .and_then(|file| file.set_modified(std::time::SystemTime::now() - 2 * CLAIM_GRACE))
      .expect("an older modification time");
  }

  #[test]
  fn removes_only_directories_whose_owner_has_gone() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());

    let unclaimed = directory(&store, "session-cb");
    age(&unclaimed);
    let running = directory(&store, "cb-builder-live");
    let _lock = claim(&running).expect("a claim");
    let gone = directory(&store, "cb-builder-dead");
    drop(claim(&gone).expect("a claim"));
    let legacy = directory(&store, "cb-builder-legacy");
    std::fs::write(legacy.join(LEGACY_BUILDER_MARKER), format!("{}\n", std::process::id())).expect("a marker");

    assert_eq!(abandoned(&store, None), std::slice::from_ref(&gone));
    assert!(unclaimed.exists(), "nothing says whose it is");
    assert!(running.exists());
    assert!(legacy.exists(), "a build from before `.owner`");
  }

  #[test]
  fn removes_an_unclaimed_directory_whose_session_does_not_answer() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path().join("images"));
    let unclaimed = directory(&store, "session-cb");
    age(&unclaimed);

    assert_eq!(abandoned(&store, Some(&root.path().join("sessions"))), [unclaimed]);
  }

  #[test]
  fn keeps_a_directory_too_young_to_have_been_claimed() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path().join("images"));
    let booting = directory(&store, "session-cb");

    assert!(abandoned(&store, Some(&root.path().join("sessions"))).is_empty());
    assert!(booting.exists());
  }

  #[test]
  fn counts_a_legacy_builder_owned_by_another_user_as_alive() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::write(root.path().join(LEGACY_BUILDER_MARKER), "1\n").expect("a marker for launchd or init");

    assert_eq!(owner_lives(root.path()), Some(true));
  }

  #[test]
  fn a_build_holds_off_reclaiming() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());
    let file = open(&root.path().join(RECLAIM_LOCK)).expect("the lock file");

    let building = building(&store).expect("a shared lock");
    assert!(matches!(file.try_lock(), Err(TryLockError::WouldBlock)));

    drop(building);
    assert!(file.try_lock().is_ok());
  }

  #[test]
  fn sweeps_nothing_without_a_store() {
    let swept = sweep(&Store::at("/nonexistent/images"), Path::new("/nonexistent/sessions"));

    assert!(swept.removed.is_empty());
    assert!(swept.blobs.is_none());
  }
}
