//! Images unpacked once, under `unpacked` in the image store.
//!
//! Each container needs its own writable ext4 rootfs, and unpacking rewrites
//! gigabytes. So each image is unpacked once, keyed by digest, and a container
//! gets a clone of it: free on APFS, costing only the blocks it changes.
//!
//! A build evicts an entry once no image in the store has its digest.

use super::files::{self, clone, partial};
use crate::error::EngineError;
use containerization_framework::containerization::{Ext4Unpacker, Image, Mount};
use containerization_framework::containerization_oci::Platform;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Ceiling for an unpack: sparse, so a ceiling, not a cost, but nothing in a
/// container may outgrow it. `ContainerManager`'s own default.
///
/// Not part of an entry's key: an image unpacked once is reused whatever
/// ceiling the next container asks for.
pub const ROOTFS_SIZE_IN_BYTES: u64 = 8 << 30;

/// An unpack takes minutes at most; an older partial was abandoned.
const ABANDONED_AFTER: Duration = Duration::from_secs(60 * 60);

/// An ext4 image file as the guest's root filesystem.
pub fn ext4_root(rootfs: &Path) -> Mount {
  Mount::block("ext4", rootfs.display().to_string(), "/", &[])
}

pub struct Unpacked {
  root: PathBuf,
}

impl Unpacked {
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  fn path(&self, digest: &str) -> PathBuf {
    self.root.join(format!("{}.ext4", digest.replace(':', "-")))
  }

  /// Whether the image is already unpacked, so a first boot can say it is
  /// about to unpack it.
  pub fn holds(&self, image: &Image) -> bool {
    self.path(&image.digest()).is_file()
  }

  /// Clones the image's unpacked rootfs to `destination`, unpacking it first on
  /// the image's first boot.
  pub fn rootfs(&self, image: &Image, platform: &Platform, destination: &Path) -> Result<Mount, EngineError> {
    let source = self.path(&image.digest());

    if !source.is_file() {
      self.unpack(image, platform, &source)?;
    }

    clone(&source, destination)
      .map_err(|error| EngineError::failed(format!("copy the rootfs of {}", image.reference()), error))?;

    Ok(ext4_root(destination))
  }

  fn unpack(&self, image: &Image, platform: &Platform, destination: &Path) -> Result<(), EngineError> {
    let failed = |error: std::io::Error| EngineError::failed(format!("unpack {}", image.reference()), error);

    std::fs::create_dir_all(&self.root).map_err(failed)?;

    let scratch = partial(&self.root).map_err(failed)?;
    let unpacked = Ext4Unpacker::new(ROOTFS_SIZE_IN_BYTES, None).unpack(image, platform, &scratch, None);
    let moved = unpacked
      .map_err(EngineError::from)
      .and_then(|_| match std::fs::rename(&scratch, destination) {
        // Another container unpacked it first; its copy is equivalent.
        Err(_) if destination.is_file() => Ok(()),
        moved => moved.map_err(failed),
      });
    let _ = std::fs::remove_file(&scratch);

    moved
  }

  /// Removes entries no stored image has the digest of, and abandoned partials.
  /// Running containers hold clones, never entries, so nothing in use is lost.
  pub fn evict(&self, keeping: impl IntoIterator<Item = String>) {
    let kept: BTreeSet<PathBuf> = keeping
      .into_iter()
      .map(|digest| self.path(&digest))
      .collect();
    let cutoff = SystemTime::now() - ABANDONED_AFTER;
    let Ok(entries) = std::fs::read_dir(&self.root) else {
      return;
    };

    for path in entries.flatten().map(|entry| entry.path()) {
      let stale = if files::is_partial(&path) {
        modified(&path).is_some_and(|modified| modified < cutoff)
      } else {
        path
          .extension()
          .is_some_and(|extension| extension == "ext4")
          && !kept.contains(&path)
      };

      if stale {
        let _ = std::fs::remove_file(&path);
      }
    }
  }
}

fn modified(path: &Path) -> Option<SystemTime> {
  std::fs::metadata(path)
    .and_then(|metadata| metadata.modified())
    .ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn evicts_what_no_stored_image_has_the_digest_of() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let unpacked = Unpacked::at(directory.path());
    let (kept, removed) = (unpacked.path("sha256:aa"), unpacked.path("sha256:bb"));
    let fresh = partial(directory.path()).expect("randomness");

    for path in [&kept, &removed, &fresh] {
      std::fs::write(path, "").expect("an entry");
    }

    unpacked.evict(["sha256:aa".to_string()]);

    assert!(kept.exists(), "a stored image's rootfs");
    assert!(!removed.exists(), "a removed image's rootfs");
    assert!(fresh.exists(), "an unpack still under way");
  }

  #[test]
  fn names_an_entry_for_its_digest() {
    assert_eq!(
      Unpacked::at("/store/unpacked").path("sha256:aa"),
      Path::new("/store/unpacked/sha256-aa.ext4")
    );
  }
}
