use super::files::{self, clone, partial, settle};
use crate::error::EngineError;
use containerization_framework::containerization::container::Mount;
use containerization_framework::containerization::image::{EXT4Unpacker, Image};
use containerization_framework::containerization_oci::image::Platform;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const ABANDONED_AFTER: Duration = Duration::from_secs(60 * 60);

pub fn ext4_root(rootfs: &Path) -> Mount {
  Mount::block("ext4", rootfs.display().to_string(), "/", &[], &[])
}

pub struct Unpacked {
  root: PathBuf,
}

impl Unpacked {
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  /// Sized too, because a clone keeps its entry's size.
  fn path(&self, digest: &str, size_in_bytes: u64) -> PathBuf {
    self
      .root
      .join(format!("{}-{size_in_bytes}.ext4", name(digest)))
  }

  pub fn holds(&self, image: &Image, size_in_bytes: u64) -> bool {
    self.path(&image.digest(), size_in_bytes).is_file()
  }

  pub fn rootfs(
    &self,
    image: &Image,
    platform: &Platform,
    size_in_bytes: u64,
    destination: &Path,
  ) -> Result<Mount, EngineError> {
    let source = self.path(&image.digest(), size_in_bytes);

    if !source.is_file() {
      self.unpack(image, platform, size_in_bytes, &source)?;
    }

    clone(&source, destination)
      .map_err(|error| EngineError::failed(format!("copy the rootfs of {}", image.reference()), error))?;

    Ok(ext4_root(destination))
  }

  fn unpack(
    &self,
    image: &Image,
    platform: &Platform,
    size_in_bytes: u64,
    destination: &Path,
  ) -> Result<(), EngineError> {
    let failed = |error: std::io::Error| EngineError::failed(format!("unpack {}", image.reference()), error);

    std::fs::create_dir_all(&self.root).map_err(failed)?;

    let scratch = partial(&self.root).map_err(failed)?;
    let moved = EXT4Unpacker::new(size_in_bytes, None)
      .unpack(image, platform, &scratch, None)
      .map_err(EngineError::from)
      .and_then(|_| settle(&scratch, destination).map_err(failed));
    let _ = std::fs::remove_file(&scratch);

    moved
  }

  /// Safe while containers run, since they hold clones, never entries.
  /// Keeps every size of a kept digest's entry.
  pub fn evict(&self, keeping: impl IntoIterator<Item = String>) -> Vec<PathBuf> {
    let kept: BTreeSet<String> = keeping.into_iter().map(|digest| name(&digest)).collect();
    let cutoff = SystemTime::now() - ABANDONED_AFTER;

    std::fs::read_dir(&self.root)
      .into_iter()
      .flatten()
      .flatten()
      .map(|entry| entry.path())
      .filter(|path| {
        if files::is_partial(path) {
          modified(path).is_some_and(|modified| modified < cutoff)
        } else {
          path
            .extension()
            .is_some_and(|extension| extension == "ext4")
            && !path
              .file_stem()
              .and_then(|stem| stem.to_str())
              .and_then(|stem| stem.rsplit_once('-'))
              .is_some_and(|(digest, _)| kept.contains(digest))
        }
      })
      .filter(|path| std::fs::remove_file(path).is_ok())
      .collect()
  }
}

fn name(digest: &str) -> String {
  digest.replace(':', "-")
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
    let (kept, resized) = (
      unpacked.path("sha256:aa", 8 << 30),
      unpacked.path("sha256:aa", 16 << 30),
    );
    let removed = unpacked.path("sha256:bb", 8 << 30);
    let fresh = partial(directory.path()).expect("randomness");

    for path in [&kept, &resized, &removed, &fresh] {
      std::fs::write(path, "").expect("an entry");
    }

    assert_eq!(
      unpacked.evict(["sha256:aa".to_string()]),
      std::slice::from_ref(&removed)
    );
    assert!(kept.exists(), "a stored image's rootfs");
    assert!(resized.exists(), "a stored image's rootfs at another size");
    assert!(!removed.exists(), "a removed image's rootfs");
    assert!(fresh.exists(), "an unpack still under way");
  }

  #[test]
  fn evicts_an_entry_named_without_a_size() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let unnamed = directory.path().join("sha256-aa.ext4");

    std::fs::write(&unnamed, "").expect("an entry");

    assert_eq!(
      Unpacked::at(directory.path()).evict(["sha256:aa".to_string()]),
      [unnamed]
    );
  }

  #[test]
  fn names_an_entry_for_its_digest_and_size() {
    assert_eq!(
      Unpacked::at("/store/unpacked").path("sha256:aa", 8 << 30),
      Path::new("/store/unpacked/sha256-aa-8589934592.ext4")
    );
  }
}
