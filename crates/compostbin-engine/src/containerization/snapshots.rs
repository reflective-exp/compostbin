use super::cache::Keys;
use super::files::{clone, partial};
use super::{note, oci};
use containerization_framework::containerization_oci::image::Descriptor;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const KEEP: usize = 24;
const KEEP_FOR: Duration = Duration::from_secs(14 * 24 * 60 * 60);

pub struct Salted {
  pub base: String,
  pub steps: Vec<String>,
  /// Also covers the user and working directory, which a rootfs does not record.
  pub image: String,
}

impl Salted {
  /// Salted with the rootfs size too, because a snapshot keeps the size it was taken at.
  pub fn new(keys: Keys, base_digest: &str, rootfs_size: u64, user: Option<&str>, workdir: Option<&Path>) -> Self {
    let salted = |key: &str| digest(&format!("{base_digest}\n{rootfs_size}\n{key}"));
    let base = salted(&keys.base);
    let steps: Vec<String> = keys.steps.iter().map(|key| salted(key)).collect();
    let image = digest(&format!(
      "{}\n{}\n{}",
      steps.last().unwrap_or(&base),
      user.unwrap_or_default(),
      workdir
        .map(|path| path.display().to_string())
        .unwrap_or_default()
    ));

    Self { base, steps, image }
  }
}

pub fn short(key: &str) -> &str {
  &key[..12.min(key.len())]
}

fn digest(text: &str) -> String {
  Sha256::digest(text.as_bytes())
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

pub struct Snapshots {
  root: PathBuf,
}

impl Snapshots {
  pub fn at(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  fn rootfs_directory(&self) -> PathBuf {
    self.root.join("rootfs")
  }

  fn images_directory(&self) -> PathBuf {
    self.root.join("images")
  }

  fn rootfs_path(&self, key: &str) -> PathBuf {
    self.rootfs_directory().join(format!("{key}.ext4"))
  }

  fn image_path(&self, key: &str) -> PathBuf {
    self.images_directory().join(format!("{key}.json"))
  }

  pub fn holds_rootfs(&self, key: &str) -> bool {
    self.rootfs_path(key).is_file()
  }

  pub fn restore(&self, key: &str, destination: &Path) -> std::io::Result<()> {
    let source = self.rootfs_path(key);

    clone(&source, destination)?;
    touch(&source);

    Ok(())
  }

  pub fn save_rootfs(&self, rootfs: &Path, key: &str) {
    let destination = self.rootfs_path(key);

    if destination.is_file() {
      touch(&destination);
      return;
    }

    let saved = std::fs::create_dir_all(self.rootfs_directory())
      .and_then(|_| partial(&self.rootfs_directory()))
      .and_then(|scratch| {
        // Cloned aside then moved, so an interrupted clone is never found.
        let moved = clone(rootfs, &scratch).and_then(|_| std::fs::rename(&scratch, &destination));
        let _ = std::fs::remove_file(&scratch);

        moved
      });

    if let Err(error) = saved {
      note(&format!("could not cache the rootfs for {}: {error}", short(key)));
    }
  }

  pub fn image(&self, key: &str) -> Option<Descriptor> {
    let path = self.image_path(key);
    let text = std::fs::read_to_string(&path).ok()?;
    touch(&path);

    oci::from_json(&text)
  }

  pub fn save_image(&self, descriptor: &Descriptor, key: &str) {
    let saved = std::fs::create_dir_all(self.images_directory())
      .and_then(|_| std::fs::write(self.image_path(key), oci::to_json(descriptor)));

    if let Err(error) = saved {
      note(&format!("could not cache the image for {}: {error}", short(key)));
    }
  }

  /// Restoring or re-saving an entry counts as use.
  pub fn evict(&self) {
    let cutoff = SystemTime::now() - KEEP_FOR;

    for (index, (path, used)) in entries(&self.rootfs_directory()).into_iter().enumerate() {
      if index >= KEEP || used < cutoff {
        let _ = std::fs::remove_file(path);
      }
    }

    for (path, used) in entries(&self.images_directory()) {
      if used < cutoff {
        let _ = std::fs::remove_file(path);
      }
    }
  }
}

fn entries(directory: &Path) -> Vec<(PathBuf, SystemTime)> {
  let mut entries: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(directory)
    .into_iter()
    .flatten()
    .flatten()
    .map(|entry| {
      let used = entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

      (entry.path(), used)
    })
    .collect();

  entries.sort_by(|(_, left), (_, right)| right.cmp(left));

  entries
}

fn touch(path: &Path) {
  if let Ok(file) = std::fs::File::options().append(true).open(path) {
    let _ = file.set_modified(SystemTime::now());
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn keys() -> Keys {
    Keys {
      base: "base".to_string(),
      steps: vec!["one".to_string(), "two".to_string()],
    }
  }

  #[test]
  fn a_moved_base_misses_every_snapshot() {
    let before = Salted::new(keys(), "sha256:aa", 8 << 30, None, None);
    let after = Salted::new(keys(), "sha256:bb", 8 << 30, None, None);

    assert_ne!(before.base, after.base);
    assert_ne!(before.steps, after.steps);
    assert_ne!(before.image, after.image);
  }

  #[test]
  fn the_image_key_covers_its_user_and_working_directory() {
    let plain = Salted::new(keys(), "sha256:aa", 8 << 30, None, None);
    let claude = Salted::new(
      keys(),
      "sha256:aa",
      8 << 30,
      Some("claude"),
      Some(Path::new("/workspace")),
    );

    assert_eq!(plain.steps, claude.steps);
    assert_ne!(plain.image, claude.image);
  }

  #[test]
  fn restores_a_saved_rootfs_and_its_image() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let snapshots = Snapshots::at(directory.path().join("build-cache"));
    let rootfs = directory.path().join("rootfs.ext4");
    let restored = directory.path().join("restored.ext4");
    let descriptor = Descriptor::new("application/vnd.oci.image.index.v1+json", "sha256:aa", 42);
    std::fs::write(&rootfs, "after step one").expect("a rootfs");

    snapshots.save_rootfs(&rootfs, "one");
    snapshots.save_image(&descriptor, "image");
    snapshots.restore("one", &restored).expect("a restore");

    assert!(snapshots.holds_rootfs("one"));
    assert!(!snapshots.holds_rootfs("two"));
    assert_eq!(std::fs::read_to_string(restored).expect("restored"), "after step one");
    assert_eq!(snapshots.image("image"), Some(descriptor));
  }
}
