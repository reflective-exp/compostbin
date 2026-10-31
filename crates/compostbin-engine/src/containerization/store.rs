//! Follows Containerization's `ImageStore` layout, which `ContainerManager` opens as-is.

use crate::error::EngineError;
use containerization_framework::containerization::image::{Image, ImageStore};
use containerization_framework::containerization_oci::content::LocalContentStore;
use containerization_framework::versions::{INIT_IMAGE, KERNEL_ARCHIVE_URL};
use std::fmt;
use std::path::{Path, PathBuf};

pub fn initfs_version() -> &'static str {
  INIT_IMAGE
    .rsplit_once(':')
    .map_or(INIT_IMAGE, |(_, tag)| tag)
}

pub fn kernel_version() -> &'static str {
  KERNEL_ARCHIVE_URL
    .rsplit('/')
    .nth(1)
    .unwrap_or(KERNEL_ARCHIVE_URL)
}

/// False unless both are dotted release numbers.
pub fn is_older(version: &str, than: &str) -> bool {
  let parse = |version: &str| {
    let mut parts = version
      .split('.')
      .map(str::parse::<u64>)
      .collect::<Result<Vec<_>, _>>()
      .ok()?;

    while parts.last() == Some(&0) {
      parts.pop();
    }

    Some(parts)
  };

  matches!((parse(version), parse(than)), (Some(version), Some(than)) if version < than)
}

const CONTAINERS: &str = "containers";
const CONTENT: &str = "content";
const KERNEL_PREFIX: &str = "vmlinux-";
const INITFS_PREFIX: &str = "vminit-";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Store {
  root: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
pub enum StoreError {
  Missing(PathBuf),
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
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Named for its version so a changed pin makes provisioning fetch the new one.
  pub fn kernel(&self) -> PathBuf {
    self
      .root
      .join(format!("kernels/{KERNEL_PREFIX}{}", kernel_version()))
  }

  pub fn initfs(&self) -> PathBuf {
    self
      .root
      .join(format!("initfs/{INITFS_PREFIX}{}.ext4", initfs_version()))
  }

  /// Kernels and init images older than the pinned ones; a newer one may belong to another compostbin sharing the store.
  pub fn retired(&self) -> Vec<PathBuf> {
    [
      ("kernels", KERNEL_PREFIX, "", kernel_version()),
      ("initfs", INITFS_PREFIX, ".ext4", initfs_version()),
    ]
    .into_iter()
    .flat_map(|(directory, prefix, suffix, current)| {
      let entries = std::fs::read_dir(self.root.join(directory)).ok();

      entries
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(move |path| {
          path.file_name().is_some_and(|name| {
            name
              .to_string_lossy()
              .strip_prefix(prefix)
              .and_then(|rest| rest.strip_suffix(suffix))
              .is_some_and(|version| is_older(version, current))
          })
        })
    })
    .collect()
  }

  pub fn containers(&self) -> PathBuf {
    self.root.join(CONTAINERS)
  }

  /// Must match the directory `ContainerManager` uses for that id.
  pub fn container_dir(&self, name: &str) -> PathBuf {
    self.containers().join(name)
  }

  pub fn build_cache(&self) -> PathBuf {
    self.root.join("build-cache")
  }

  pub fn unpacked(&self) -> PathBuf {
    self.root.join("unpacked")
  }

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

  pub fn images(&self) -> Result<ImageStore, EngineError> {
    Ok(ImageStore::new(&self.root)?)
  }

  pub fn content(&self) -> Result<(LocalContentStore, ImageStore), EngineError> {
    let content = LocalContentStore::new(&self.root.join(CONTENT))?;
    let images = ImageStore::with_content_store(&self.root, &content)?;

    Ok((content, images))
  }

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
      Path::new(&format!("/images/kernels/vmlinux-{}", kernel_version()))
    );
    assert_eq!(
      store.initfs(),
      Path::new(&format!("/images/initfs/vminit-{}.ext4", initfs_version()))
    );
  }

  #[test]
  fn reads_each_version_off_the_framework_pins() {
    for version in [initfs_version(), kernel_version()] {
      assert!(
        version
          .split('.')
          .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())),
        "{version} should be a dotted release number"
      );
    }

    assert!(INIT_IMAGE.ends_with(&format!(":{}", initfs_version())));
    assert!(KERNEL_ARCHIVE_URL.ends_with(&format!("/kata-static-{}-arm64.tar.xz", kernel_version())));
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
  fn retires_only_kernels_and_init_images_older_than_the_pins() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());
    let file = |relative: &str| {
      let path = root.path().join(relative);
      std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
      std::fs::write(&path, "").expect("a file");
      path
    };

    file(&format!("kernels/vmlinux-{}", kernel_version()));
    file(&format!("initfs/vminit-{}.ext4", initfs_version()));
    let old_kernel = file("kernels/vmlinux-0.0.1");
    let old_initfs = file("initfs/vminit-0.0.1.ext4");
    file("kernels/vmlinux-999.0.0");
    file("initfs/vminit-999.0.0.ext4");
    file("kernels/0123456789abcdef.partial");
    file("initfs/0123456789abcdef.partial");
    file("initfs/unrelated");

    let mut retired = store.retired();
    retired.sort();

    assert_eq!(retired, [old_initfs, old_kernel]);
  }

  #[test]
  fn compares_release_numbers_part_by_part() {
    assert!(is_older("0.9.0", "0.10.0"));
    assert!(is_older("0.54", "0.54.1"));
    assert!(!is_older("0.54", "0.54.0"));
    assert!(!is_older("0.54.0", "0.54"));
    assert!(!is_older("0.54.0", "0.54.0"));
    assert!(!is_older("0.55.0", "0.54.0"));
    assert!(!is_older("latest", "0.54.0"));
  }

  #[test]
  fn retires_nothing_from_an_empty_store() {
    assert_eq!(Store::at("/nonexistent/images").retired(), Vec::<PathBuf>::new());
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
