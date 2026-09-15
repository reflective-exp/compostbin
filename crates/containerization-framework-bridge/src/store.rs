//! Where the boot artefacts live.
//!
//! compostbin does not keep a store of its own. Containerization's `ImageStore`
//! is exactly what the `container` CLI already writes — an image index in
//! `state.json`, blobs under `content/blobs/sha256`, per-container rootfs under
//! `containers/<id>` — so we point at the CLI's directory and read what
//! `compostbin build` has already put there.
//!
//! That is a coupling to an unpublished layout, and it is deliberate:
//! Containerization has no image builder, so `build` has to stay on the CLI
//! whatever else moves.

use std::path::{Path, PathBuf};

/// Under the user's Application Support directory.
const STORE_DIR: &str = "Library/Application Support/com.apple.container";
/// The CLI writes both a versioned kernel and this stable alias.
const KERNEL: &str = "kernels/default.kernel-arm64";
/// The image index: a map of reference to OCI descriptor.
const INDEX: &str = "state.json";
/// One directory per container, named after it.
const CONTAINERS: &str = "containers";

/// The initfs carrying `vminitd`, the agent the library talks to over vsock.
///
/// Pinned, and pinned to the same release as the `containerization` dependency
/// in `swift/CompostbinContainerization/Package.swift`. The two are one
/// protocol: a library newer than the agent on disk fails at runtime, not at
/// build time, so these must be changed together.
pub const INITFS_VERSION: &str = "0.45.0";
pub const INITFS_REFERENCE: &str = "ghcr.io/apple/containerization/vminit:0.45.0";

#[derive(Clone, Debug, PartialEq)]
pub struct Store {
  root: PathBuf,
}

#[derive(Debug, PartialEq)]
pub enum StoreError {
  /// No `container` store at all — nothing has ever been built.
  Missing(PathBuf),
  /// A store, but without the kernel or the initfs a boot needs. A CLI too old
  /// to have written them, or one whose `container system start` has not run.
  Incomplete { root: PathBuf, missing: String },
}

impl std::fmt::Display for StoreError {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Missing(root) => write!(
        formatter,
        "no container store at {}; run `compostbin build`",
        root.display()
      ),
      Self::Incomplete { root, missing } => {
        write!(formatter, "the container store at {} has no {missing}", root.display())
      }
    }
  }
}

impl std::error::Error for StoreError {}

impl Store {
  /// The store the `container` CLI keeps for this user.
  pub fn discover() -> Result<Self, StoreError> {
    let home = std::env::var("HOME").unwrap_or_default();

    Self::at(Path::new(&home).join(STORE_DIR))
  }

  /// Checks a store holds what a boot needs, naming the first thing it does not.
  pub fn at(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
    let root = root.into();

    if !root.is_dir() {
      return Err(StoreError::Missing(root));
    }

    let store = Self { root };

    if !store.kernel().is_file() {
      return Err(StoreError::Incomplete {
        root: store.root,
        missing: format!("kernel at {KERNEL}"),
      });
    }

    if !store.holds(INITFS_REFERENCE) {
      return Err(StoreError::Incomplete {
        root: store.root,
        missing: format!("{INITFS_REFERENCE} in {INDEX}"),
      });
    }

    Ok(store)
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn kernel(&self) -> PathBuf {
    self.root.join(KERNEL)
  }

  /// Where a container's own files go: its unpacked rootfs, its copy of the
  /// kernel, its boot log.
  pub fn container_dir(&self, name: &str) -> PathBuf {
    self.root.join(CONTAINERS).join(name)
  }

  pub fn initfs_reference(&self) -> &'static str {
    INITFS_REFERENCE
  }

  /// Whether the index names an image. A substring test, not a parse: the index
  /// is keyed by reference, and a reference cannot occur anywhere else in it.
  ///
  /// The index is written by Foundation, which escapes every forward slash as
  /// `\/` — so a registry-qualified reference never matches as written and the
  /// slashes have to come back first.
  pub fn holds(&self, reference: &str) -> bool {
    std::fs::read_to_string(self.root.join(INDEX))
      .map(|index| {
        index
          .replace("\\/", "/")
          .contains(&format!("\"{reference}\""))
      })
      .unwrap_or(false)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn refuses_a_directory_that_is_not_a_store() {
    assert_eq!(
      Store::at("/nonexistent/container"),
      Err(StoreError::Missing(PathBuf::from("/nonexistent/container")))
    );
  }

  #[test]
  fn names_the_reference_it_could_not_find() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(root.path().join("kernels")).expect("kernels");
    std::fs::write(root.path().join(KERNEL), "").expect("kernel");
    std::fs::write(root.path().join(INDEX), "{}").expect("index");

    let error = Store::at(root.path()).expect_err("an incomplete store");

    assert!(
      error.to_string().contains(INITFS_REFERENCE),
      "error should name the missing image: {error}"
    );
  }

  /// As Foundation writes it: every forward slash escaped. Matching the real
  /// file rather than a tidied version of it is the whole point — the tidied
  /// one passed while the real one did not.
  fn index_holding(reference: &str) -> String {
    format!("{{\"{}\":{{}}}}", reference.replace('/', "\\/"))
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
  fn accepts_a_store_holding_the_kernel_and_the_initfs() {
    let root = tempfile::tempdir().expect("a temp dir");
    std::fs::create_dir_all(root.path().join("kernels")).expect("kernels");
    std::fs::write(root.path().join(KERNEL), "").expect("kernel");
    std::fs::write(root.path().join(INDEX), index_holding(INITFS_REFERENCE)).expect("index");

    let store = Store::at(root.path()).expect("a complete store");

    assert_eq!(store.root(), root.path());
    assert_eq!(store.kernel(), root.path().join(KERNEL));
    assert_eq!(
      store.container_dir("compostbin-cb"),
      root.path().join("containers/compostbin-cb")
    );
  }
}
