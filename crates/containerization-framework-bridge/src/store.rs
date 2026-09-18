//! Where the boot artefacts live.
//!
//! Containerization's `ImageStore` layout: an image index in `state.json`, blobs
//! under `content/blobs/sha256`, the kernel under `kernels`, and one rootfs per
//! container under `containers/<id>`. `ContainerManager` opens the directory
//! as-is, so the layout is the library's rather than anything invented here.
//!
//! Two additions are ours: `build-cache`, the builder's step snapshots, and
//! `unpacked`, each image's rootfs unpacked once and cloned into every container
//! that runs it.
//!
//! Nothing in it is precious: `provision` fetches the kernel and the init image
//! when they are missing, and `build` makes the images again. Which is why it
//! lives under `~/.cache`.

use std::fmt;
use std::path::{Path, PathBuf};

/// The kernel a session boots, under the store root.
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

/// The kernel `provision` fetches, and where it sits inside that archive.
///
/// Kata Containers' static build, the same release Containerization's own Makefile
/// pins. A download rather than a pull: nobody publishes a kernel as an image.
pub const KERNEL_VERSION: &str = "3.17.0";
pub const KERNEL_URL: &str =
  "https://github.com/kata-containers/kata-containers/releases/download/3.17.0/kata-static-3.17.0-arm64.tar.xz";
pub const KERNEL_IN_ARCHIVE: &str = "opt/kata/share/kata-containers/vmlinux.container";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
  root: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
pub enum StoreError {
  /// No store at all: nothing has ever been provisioned here.
  Missing(PathBuf),
  /// A store, but without the kernel or the init image a boot needs.
  Incomplete { root: PathBuf, missing: String },
  /// The index is there and cannot be read.
  Unreadable { path: PathBuf, source: String },
}

impl fmt::Display for StoreError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(root) => write!(
        formatter,
        "no image store at {}; run `compostbin build`",
        root.display()
      ),
      Self::Incomplete { root, missing } => write!(
        formatter,
        "the image store at {} has no {missing}; run `compostbin build`",
        root.display()
      ),
      Self::Unreadable { path, source } => write!(formatter, "cannot read {}: {source}", path.display()),
    }
  }
}

impl std::error::Error for StoreError {}

impl Store {
  /// Names a store without looking at it. Nothing may exist there yet: a first
  /// `build` provisions it, and until then every path below is a path it *will*
  /// have.
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  /// Whether this store holds what a boot needs, naming the first thing it does
  /// not. What `run` and `doctor` ask before they get any further.
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

  /// Where a container's own files go: its unpacked rootfs, its copy of the
  /// kernel, its boot log.
  pub fn container_dir(&self, name: &str) -> PathBuf {
    self.root.join(CONTAINERS).join(name)
  }

  pub fn initfs_reference(&self) -> &'static str {
    INITFS_REFERENCE
  }

  /// Every image the store holds, as `name:tag`. A store with no index yet holds
  /// none, which is a fact rather than a failure — it is what a first run looks
  /// like, and `doctor` says so better than an unreadable-file error would.
  ///
  /// The index is a JSON object keyed by reference, so the keys are the answer.
  /// Read by hand rather than parsed: one level of keys is all that is wanted,
  /// and the values are OCI descriptors this has no use for.
  pub fn images(&self) -> Result<Vec<String>, StoreError> {
    let index = self.root.join(INDEX);

    if !index.exists() {
      return Ok(Vec::new());
    }

    let text = std::fs::read_to_string(&index)
      .map_err(|source| StoreError::Unreadable {
        path: index,
        source: source.to_string(),
      })?
      .replace("\\/", "/");

    let mut references: Vec<String> = text
      .match_indices("\":{")
      .filter_map(|(at, _)| {
        text[..at]
          .rfind('"')
          .map(|start| text[start + 1..at].to_string())
      })
      // Nested objects are keyed too — annotations above all — and their keys
      // are not references. A reference always carries a tag.
      .filter(|reference| reference.contains(':'))
      .collect();

    references.sort();
    references.dedup();

    Ok(references)
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

  /// What a first run looks like: a directory and nothing in it.
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

  /// As Foundation writes it: every forward slash escaped. Matching the real
  /// file rather than a tidied version of it is the whole point — the tidied
  /// one passed while the real one did not.
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
      r#"{"compostbin\/base:latest":{"digest":"sha256:aa","annotations":{"org.opencontainers.image.ref.name":"latest"}},"docker.io\/library\/debian:stable-slim":{"digest":"sha256:bb"}}"#,
    )
    .expect("index");

    let store = Store {
      root: root.path().to_path_buf(),
    };

    assert_eq!(
      store.images().expect("the index should be readable"),
      ["compostbin/base:latest", "docker.io/library/debian:stable-slim"]
    );
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
      store.container_dir("compostbin-cb"),
      root.path().join("containers/compostbin-cb")
    );
  }
}
