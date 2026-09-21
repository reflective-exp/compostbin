//! Where the boot artefacts live.
//!
//! Containerization's `ImageStore` layout, which `ContainerManager` opens as-is:
//! an image index in `state.json`, blobs under `content/blobs/sha256`, the
//! kernel under `kernels`, and one rootfs per container under `containers/<id>`.
//!
//! Ours: `build-cache` (the builder's step snapshots) and `unpacked` (each
//! image's rootfs, unpacked once and cloned per container).
//!
//! Everything is re-creatable by `provision` and `build`, hence `~/.cache`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

const KERNEL: &str = "kernels/default.kernel-arm64";
/// Map of reference to OCI descriptor.
const INDEX: &str = "state.json";
const CONTAINERS: &str = "containers";

/// The initfs carrying `vminitd`, the agent the library talks to over vsock.
///
/// Must match the `containerization` release in
/// this crate's `swift/Package.swift`: they share a protocol,
/// and a mismatch fails at runtime, not build time.
macro_rules! initfs_version {
  () => {
    "0.45.0"
  };
}

macro_rules! kernel_version {
  () => {
    "3.17.0"
  };
}

pub const INITFS_VERSION: &str = initfs_version!();
pub const INITFS_REFERENCE: &str = concat!("ghcr.io/apple/containerization/vminit:", initfs_version!());

/// The kernel `provision` fetches, and its path inside the archive.
///
/// Kata Containers' static build, the release Containerization's Makefile
/// pins. Downloaded, since no one publishes a kernel as an image.
pub const KERNEL_VERSION: &str = kernel_version!();

/// Where the kernel comes from. Only the provisioner reads these, and only
/// macOS has one.
#[cfg(target_os = "macos")]
pub const KERNEL_URL: &str = concat!(
  "https://github.com/kata-containers/kata-containers/releases/download/",
  kernel_version!(),
  "/kata-static-",
  kernel_version!(),
  "-arm64.tar.xz"
);
#[cfg(target_os = "macos")]
pub const KERNEL_IN_ARCHIVE: &str = "opt/kata/share/kata-containers/vmlinux.container";

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
  /// The index exists but can't be read.
  Unreadable { path: PathBuf, source: String },
}

impl fmt::Display for StoreError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(root) => write!(formatter, "no image store at {}", root.display()),
      Self::Incomplete { root, missing } => {
        write!(formatter, "the image store at {} has no {missing}", root.display())
      }
      Self::Unreadable { path, source } => write!(formatter, "cannot read {}: {source}", path.display()),
    }
  }
}

impl std::error::Error for StoreError {}

impl Store {
  /// Names a store without touching it; the first `build` provisions it.
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  /// Whether this store holds what a boot needs, naming the first thing
  /// missing. Worth asking before a boot, which would fail late instead.
  pub fn ready(&self) -> Result<(), StoreError> {
    if !self.root.is_dir() {
      return Err(StoreError::Missing(self.root.clone()));
    }

    if !self.kernel().is_file() {
      return Err(StoreError::Incomplete {
        root: self.root.clone(),
        missing: format!("kernel at {KERNEL}"),
      });
    }

    if !self.holds(INITFS_REFERENCE) {
      return Err(StoreError::Incomplete {
        root: self.root.clone(),
        missing: format!("{INITFS_REFERENCE} in {INDEX}"),
      });
    }

    Ok(())
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn kernel(&self) -> PathBuf {
    self.root.join(KERNEL)
  }

  /// A container's rootfs, kernel copy, and boot log.
  pub fn container_dir(&self, name: &str) -> PathBuf {
    self.root.join(CONTAINERS).join(name)
  }

  /// Every image the store holds, as `name:tag`, sorted. No index yet means
  /// none, not an error: that's a first run.
  pub fn images(&self) -> Result<Vec<String>, StoreError> {
    let index = self.root.join(INDEX);

    if !index.exists() {
      return Ok(Vec::new());
    }

    let unreadable = |source: String| StoreError::Unreadable {
      path: index.clone(),
      source,
    };
    let text = std::fs::read_to_string(&index).map_err(|source| unreadable(source.to_string()))?;
    let references: BTreeMap<String, serde_json::Value> =
      serde_json::from_str(&text).map_err(|source| unreadable(source.to_string()))?;

    Ok(references.into_keys().collect())
  }

  /// Whether the index names an image. An unreadable index names none.
  pub fn holds(&self, reference: &str) -> bool {
    self
      .images()
      .is_ok_and(|references| references.iter().any(|held| held == reference))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn is_not_ready_before_anything_has_provisioned_it() {
    assert_eq!(
      Store::at("/nonexistent/images").ready(),
      Err(StoreError::Missing(PathBuf::from("/nonexistent/images")))
    );
  }

  #[test]
  fn names_the_reference_it_could_not_find() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(root.path().join("kernels")).expect("kernels");
    std::fs::write(root.path().join(KERNEL), "").expect("kernel");
    std::fs::write(root.path().join(INDEX), "{}").expect("index");

    let error = Store::at(root.path())
      .ready()
      .expect_err("an incomplete store");

    assert!(
      error.to_string().contains(INITFS_REFERENCE),
      "error should name the missing image: {error}"
    );
  }

  /// A first run: an empty directory.
  #[test]
  fn holds_no_images_before_the_first_build() {
    let root = tempfile::tempdir().expect("a temp dir");

    assert_eq!(
      Store::at(root.path())
        .images()
        .expect("an empty store is readable"),
      Vec::<String>::new()
    );
  }

  /// As Foundation writes it, slashes escaped. An unescaped fixture hid a bug.
  fn index_holding(reference: &str) -> String {
    format!("{{\"{}\":{{}}}}", reference.replace('/', "\\/"))
  }

  /// As Containerization writes it: nested objects, escaped slashes, and an
  /// `annotations` key that is not an image.
  #[test]
  fn lists_the_references_the_index_names() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::write(
      root.path().join(INDEX),
      r#"{"example\/base:latest":{"digest":"sha256:aa","annotations":{"org.opencontainers.image.ref.name":"latest"}},"docker.io\/library\/debian:stable-slim":{"digest":"sha256:bb"}}"#,
    )
    .expect("index");

    let store = Store {
      root: root.path().to_path_buf(),
    };

    assert_eq!(
      store.images().expect("the index should be readable"),
      ["docker.io/library/debian:stable-slim", "example/base:latest"]
    );
  }

  #[test]
  fn names_the_index_it_could_not_parse() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::write(root.path().join(INDEX), "{\"truncated").expect("index");

    let store = Store::at(root.path());

    assert!(matches!(
      store.images(),
      Err(StoreError::Unreadable { path, .. }) if path == root.path().join(INDEX)
    ));
    assert!(!store.holds(INITFS_REFERENCE));
  }

  #[test]
  fn finds_a_reference_whose_slashes_are_escaped() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::write(root.path().join(INDEX), index_holding(INITFS_REFERENCE)).expect("index");

    let store = Store {
      root: root.path().to_path_buf(),
    };

    assert!(store.holds(INITFS_REFERENCE));
    assert!(!store.holds("ghcr.io/apple/containerization/vminit:0.0.0"));
  }

  #[test]
  fn is_ready_when_it_holds_the_kernel_and_the_initfs() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(root.path().join("kernels")).expect("kernels");
    std::fs::write(root.path().join(KERNEL), "").expect("kernel");
    std::fs::write(root.path().join(INDEX), index_holding(INITFS_REFERENCE)).expect("index");

    let store = Store::at(root.path());

    store.ready().expect("a complete store");
    assert_eq!(store.root(), root.path());
    assert_eq!(store.kernel(), root.path().join(KERNEL));
    assert_eq!(
      store.container_dir("session-cb"),
      root.path().join("containers/session-cb")
    );
  }
}
