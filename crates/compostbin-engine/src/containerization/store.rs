//! Where the boot artefacts live, and which kernel and init image boot.
//!
//! Containerization's `ImageStore` layout, which `ContainerManager` opens as-is:
//! an image index in `state.json`, blobs under `content`, and one directory per
//! container under `containers/<id>`.
//!
//! Ours: `kernels` and `initfs` (what every VM boots, see [`super::provision`]),
//! `unpacked` (each image's rootfs, unpacked once and cloned per container), and
//! `build-cache` (the builder's step snapshots).
//!
//! Everything is re-creatable by `provision` and `build`, hence `~/.cache`.

use crate::error::EngineError;
use containerization_framework::containerization::image::{Image, ImageStore};
use containerization_framework::containerization_oci::content::LocalContentStore;
use std::fmt;
use std::path::{Path, PathBuf};

/// The init image release booted: the initfs carrying `vminitd`, the agent
/// Containerization talks to over vsock.
///
/// Must match the Containerization release `containerization-framework` builds:
/// they share a protocol, and a mismatch fails at runtime, not build time.
macro_rules! initfs_version {
  () => {
    "0.49.0"
  };
}

macro_rules! kernel_version {
  () => {
    "3.17.0"
  };
}

pub const INITFS_VERSION: &str = initfs_version!();
pub const INITFS_REFERENCE: &str = concat!("ghcr.io/apple/containerization/vminit:", initfs_version!());

/// Kata Containers' static build, the release Containerization's Makefile pins.
/// Downloaded, since no one publishes a kernel as an image.
pub const KERNEL_VERSION: &str = kernel_version!();

/// The archive [`KERNEL_VERSION`] is published in.
pub const KERNEL_URL: &str = concat!(
  "https://github.com/kata-containers/kata-containers/releases/download/",
  kernel_version!(),
  "/kata-static-",
  kernel_version!(),
  "-arm64.tar.xz"
);

/// The kernel's path inside [`KERNEL_URL`]'s archive. A symlink to the
/// versioned kernel beside it.
pub const KERNEL_IN_ARCHIVE: &str = "opt/kata/share/kata-containers/vmlinux.container";

const CONTAINERS: &str = "containers";
const CONTENT: &str = "content";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
  root: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
pub enum StoreError {
  /// Never provisioned.
  Missing(PathBuf),
  /// Lacks the kernel or init image a boot needs.
  Incomplete { root: PathBuf, missing: String },
}

impl fmt::Display for StoreError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(root) => write!(formatter, "no image store at {}", root.display()),
      Self::Incomplete { root, missing } => {
        write!(formatter, "the image store at {} has no {missing}", root.display())
      }
    }
  }
}

impl std::error::Error for StoreError {}

impl Store {
  /// Names a store without touching it; the first `build` provisions it.
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  /// The kernel every VM boots.
  ///
  /// Named for its version, as is the init image, so an upgrade that moves a
  /// pin finds nothing at the new path and provisioning fetches it, rather
  /// than booting what an older release left behind.
  pub fn kernel(&self) -> PathBuf {
    self.root.join(format!("kernels/vmlinux-{KERNEL_VERSION}"))
  }

  /// Where [`INITFS_REFERENCE`] is unpacked, and booted from.
  pub fn initfs(&self) -> PathBuf {
    self
      .root
      .join(format!("initfs/vminit-{INITFS_VERSION}.ext4"))
  }

  /// One directory per container, builders' and sessions' alike.
  pub fn containers(&self) -> PathBuf {
    self.root.join(CONTAINERS)
  }

  /// A container's rootfs and boot log: the directory `ContainerManager` uses
  /// for that id.
  pub fn container_dir(&self, name: &str) -> PathBuf {
    self.containers().join(name)
  }

  /// Where a build keeps its step snapshots.
  pub fn build_cache(&self) -> PathBuf {
    self.root.join("build-cache")
  }

  /// Where each image is unpacked once.
  pub fn unpacked(&self) -> PathBuf {
    self.root.join("unpacked")
  }

  /// Whether this store holds what a boot needs, naming the first thing
  /// missing. Worth asking before a boot, which would fail late instead.
  pub fn ready(&self) -> Result<(), StoreError> {
    if !self.root.is_dir() {
      return Err(StoreError::Missing(self.root.clone()));
    }

    for (what, path) in [("kernel", self.kernel()), ("init image", self.initfs())] {
      if !path.is_file() {
        return Err(StoreError::Incomplete {
          root: self.root.clone(),
          missing: format!("{what} at {}", path.display()),
        });
      }
    }

    Ok(())
  }

  /// The store's images, as Containerization opens them.
  pub fn images(&self) -> Result<ImageStore, EngineError> {
    Ok(ImageStore::new(&self.root)?)
  }

  /// The store's blobs, and its images over those same blobs: what a build
  /// writes into, then names.
  pub fn content(&self) -> Result<(LocalContentStore, ImageStore), EngineError> {
    let content = LocalContentStore::new(&self.root.join(CONTENT))?;
    let images = ImageStore::with_content_store(&self.root, &content)?;

    Ok((content, images))
  }

  /// Every image the store holds, as `name:tag`, sorted. No store yet means
  /// none, not an error: that's a first run.
  pub fn references(&self) -> Result<Vec<String>, EngineError> {
    if !self.root.is_dir() {
      return Ok(Vec::new());
    }

    let mut references: Vec<String> = self
      .images()?
      .list()?
      .iter()
      .map(Image::reference)
      .collect();

    references.sort();

    Ok(references)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_the_kernel_and_init_image_for_the_versions_it_pins() {
    let store = Store::at("/images");

    assert_eq!(
      store.kernel(),
      Path::new(&format!("/images/kernels/vmlinux-{KERNEL_VERSION}"))
    );
    assert_eq!(
      store.initfs(),
      Path::new(&format!("/images/initfs/vminit-{INITFS_VERSION}.ext4"))
    );
    assert!(INITFS_REFERENCE.ends_with(INITFS_VERSION));
  }

  #[test]
  fn is_not_ready_before_anything_has_provisioned_it() {
    assert_eq!(
      Store::at("/nonexistent/images").ready(),
      Err(StoreError::Missing(PathBuf::from("/nonexistent/images")))
    );
  }

  #[test]
  fn names_the_kernel_it_could_not_find() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());

    let error = store.ready().expect_err("a store with no kernel");

    assert!(
      error
        .to_string()
        .contains(&store.kernel().display().to_string()),
      "error should name the kernel's path: {error}"
    );
  }

  #[test]
  fn names_the_init_image_it_could_not_find() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());
    std::fs::create_dir_all(store.kernel().parent().expect("a kernels directory")).expect("kernels");
    std::fs::write(store.kernel(), "").expect("kernel");

    let error = store.ready().expect_err("a store with no init image");

    assert!(
      error
        .to_string()
        .contains(&store.initfs().display().to_string()),
      "error should name the init image's path: {error}"
    );
  }

  #[test]
  fn is_ready_when_it_holds_the_kernel_and_the_initfs() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());

    for path in [store.kernel(), store.initfs()] {
      std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
      std::fs::write(path, "").expect("a file");
    }

    store.ready().expect("a complete store");
    assert_eq!(
      store.container_dir("session-cb"),
      root.path().join("containers/session-cb")
    );
  }

  #[test]
  fn holds_no_images_before_the_first_build() {
    assert_eq!(
      Store::at("/nonexistent/images")
        .references()
        .expect("a missing store holds nothing"),
      Vec::<String>::new()
    );
  }
}
