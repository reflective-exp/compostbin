//! Cache keys for a build: one per step, covering everything that has gone into
//! the rootfs by the time that step finishes. Two builds that agree up to step
//! `i` agree on `keys.steps[i]`, so the second starts from the first's rootfs.
//!
//! A key covers the environment and every step's script and user so far. It
//! leaves out:
//!
//!   * step names — a label, and renaming one shouldn't cost a rebuild;
//!   * a mount's contents, for steps whose script doesn't name where it lands,
//!     so editing a file one step installs doesn't re-run `apt-get`;
//!   * the base's digest, which only the builder knows and mixes in itself;
//!   * anything that never reaches the rootfs: the image's user and workdir, the
//!     builder's cpus and memory.

use crate::error::EngineError;
use crate::model::{BuildMount, BuildPlan};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keys {
  /// Before any step has run.
  pub base: String,
  /// `steps[i]`: after `plan.steps[i]` has run.
  pub steps: Vec<String>,
}

/// Reads a mount only if some step names where it lands.
pub fn keys(plan: &BuildPlan) -> Result<Keys, EngineError> {
  let mut digests = Vec::with_capacity(plan.mounts.len());

  for mount in &plan.mounts {
    let read = plan.steps.iter().any(|step| reads(&step.script, mount));

    digests.push(if read { digest_of(&mount.source)? } else { String::new() });
  }

  let mut key = Key::new();

  key.field(b"compostbin/build/1");

  for variable in &plan.environment {
    key.field(variable.as_bytes());
  }

  let base = key.clone().finish();
  let mut steps = Vec::with_capacity(plan.steps.len());

  for step in &plan.steps {
    key.field(step.script.as_bytes());
    key.field(step.user.as_deref().unwrap_or("root").as_bytes());

    for (mount, digest) in plan.mounts.iter().zip(&digests) {
      if reads(&step.script, mount) {
        key.field(digest.as_bytes());
      }
    }

    steps.push(key.clone().finish());
  }

  Ok(Keys { base, steps })
}

/// A step reads a mount iff its script names where that mount lands.
fn reads(script: &str, mount: &BuildMount) -> bool {
  script.contains(&mount.destination)
}

/// One digest over every file under a mount's source. Sorted, because
/// `read_dir` order is the filesystem's.
fn digest_of(root: &Path) -> Result<String, EngineError> {
  let mut entries = Vec::new();

  collect(root, root, &mut entries)?;
  entries.sort();

  let mut key = Key::new();

  for (path, executable, contents) in entries {
    key.field(path.as_bytes());
    key.field(if executable { b"755" } else { b"644" });
    key.field(&contents);
  }

  Ok(key.finish())
}

/// Every file under `directory`: path relative to `root`, executable bit,
/// contents. A symlink hashes as its target path, not what it points at.
fn collect(root: &Path, directory: &Path, into: &mut Vec<(String, bool, Vec<u8>)>) -> Result<(), EngineError> {
  let unreadable = |path: &Path, error: std::io::Error| {
    EngineError::unavailable(format!("read the mounted directory at {}", path.display()), error)
  };

  let listing = fs::read_dir(directory).map_err(|error| unreadable(directory, error))?;

  for entry in listing {
    let entry = entry.map_err(|error| unreadable(directory, error))?;
    let path = entry.path();
    let kind = entry
      .file_type()
      .map_err(|error| unreadable(&path, error))?;

    if kind.is_dir() {
      collect(root, &path, into)?;
      continue;
    }

    let relative = path
      .strip_prefix(root)
      .unwrap_or(&path)
      .to_string_lossy()
      .into_owned();

    if kind.is_symlink() {
      let target = fs::read_link(&path).map_err(|error| unreadable(&path, error))?;

      into.push((relative, false, target.to_string_lossy().into_owned().into_bytes()));
      continue;
    }

    let mode = entry
      .metadata()
      .map_err(|error| unreadable(&path, error))?
      .permissions()
      .mode();

    into.push((
      relative,
      mode & 0o111 != 0,
      fs::read(&path).map_err(|error| unreadable(&path, error))?,
    ));
  }

  Ok(())
}

/// A SHA-256 over length-prefixed fields, so `ab`,`c` and `a`,`bc` differ.
/// Cloned to take each key while the chain keeps running.
#[derive(Clone)]
struct Key(Sha256);

impl Key {
  fn new() -> Self {
    Self(Sha256::new())
  }

  fn field(&mut self, value: &[u8]) -> &mut Self {
    self.0.update((value.len() as u64).to_le_bytes());
    self.0.update(value);
    self
  }

  fn finish(self) -> String {
    self
      .0
      .finalize()
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{BuildStep, Resources};
  use std::path::PathBuf;

  fn plan() -> BuildPlan {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    plan.environment = vec!["CLAUDE_CONFIG_DIR=/home/claude/.claude".to_string()];
    plan.steps = vec![
      BuildStep::root("packages", "apt-get update"),
      BuildStep::as_user("bashrc", "claude", "echo hook >> ~/.bashrc"),
    ];

    plan
  }

  #[test]
  fn gives_one_key_per_step_and_one_for_the_base() {
    let keys = keys(&plan()).expect("a plan with no context should key");

    assert_eq!(keys.steps.len(), 2);
    assert_ne!(keys.base, keys.steps[0]);
    assert_ne!(keys.steps[0], keys.steps[1]);
  }

  #[test]
  fn is_the_same_chain_for_the_same_plan() {
    assert_eq!(keys(&plan()).expect("a key"), keys(&plan()).expect("a key"));
  }

  #[test]
  fn changing_a_step_changes_that_key_and_every_one_after_it() {
    let before = keys(&plan()).expect("a key");

    let mut edited = plan();
    edited.steps[0] = BuildStep::root("packages", "apt-get update && apt-get install -y curl");
    let after = keys(&edited).expect("a key");

    assert_eq!(before.base, after.base);
    assert_ne!(before.steps[0], after.steps[0]);
    assert_ne!(before.steps[1], after.steps[1]);
  }

  #[test]
  fn changing_the_last_step_leaves_the_earlier_keys_alone() {
    let before = keys(&plan()).expect("a key");

    let mut edited = plan();
    edited.steps[1] = BuildStep::as_user("bashrc", "claude", "echo other >> ~/.bashrc");
    let after = keys(&edited).expect("a key");

    assert_eq!(before.steps[0], after.steps[0]);
    assert_ne!(before.steps[1], after.steps[1]);
  }

  #[test]
  fn a_renamed_step_keeps_its_key() {
    let mut renamed = plan();
    renamed.steps[0] = BuildStep::root("debian packages", "apt-get update");

    assert_eq!(keys(&plan()).expect("a key"), keys(&renamed).expect("a key"));
  }

  #[test]
  fn the_user_a_step_runs_as_is_part_of_its_key() {
    let mut elevated = plan();
    elevated.steps[1] = BuildStep::root("bashrc", "echo hook >> ~/.bashrc");

    assert_ne!(
      keys(&plan()).expect("a key").steps[1],
      keys(&elevated).expect("a key").steps[1]
    );
  }

  #[test]
  fn the_environment_is_part_of_every_key() {
    let mut other = plan();
    other.environment = vec!["CLAUDE_CONFIG_DIR=/elsewhere".to_string()];

    let before = keys(&plan()).expect("a key");
    let after = keys(&other).expect("a key");

    assert_ne!(before.base, after.base);
    assert_ne!(before.steps[0], after.steps[0]);
  }

  #[test]
  fn does_not_confuse_two_steps_with_one_of_their_two_scripts_joined() {
    let mut split = plan();
    split.steps = vec![BuildStep::root("a", "ab"), BuildStep::root("b", "c")];

    let mut joined = plan();
    joined.steps = vec![BuildStep::root("a", "a"), BuildStep::root("b", "bc")];

    assert_ne!(
      keys(&split).expect("a key").steps[1],
      keys(&joined).expect("a key").steps[1]
    );
  }

  /// Where `with_mount` lands what it shares.
  const MOUNT: &str = "/mnt/scripts";

  fn context_with(contents: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temp dir");

    std::fs::create_dir_all(directory.path().join("nested")).expect("a nested directory");
    std::fs::write(directory.path().join("relay"), contents).expect("a script");
    std::fs::write(directory.path().join("nested/other"), "other").expect("another file");

    directory
  }

  fn mounting(source: impl Into<PathBuf>) -> BuildMount {
    BuildMount {
      destination: MOUNT.to_string(),
      readonly: true,
      source: source.into(),
    }
  }

  fn with_mount(directory: &tempfile::TempDir, script: &str) -> BuildPlan {
    let mut plan = plan();

    plan.mounts = vec![mounting(directory.path())];
    plan.steps = vec![
      BuildStep::root("packages", "apt-get update"),
      BuildStep::root("guest scripts", script),
    ];

    plan
  }

  #[test]
  fn a_mount_is_part_of_the_key_of_a_step_that_reads_it() {
    let reads = format!("install -m 755 {MOUNT}/relay /usr/local/bin/");
    let before = context_with("one");
    let after = context_with("two");

    let first = keys(&with_mount(&before, &reads)).expect("a key");
    let second = keys(&with_mount(&after, &reads)).expect("a key");

    assert_eq!(first.steps[0], second.steps[0], "the step that does not read it");
    assert_ne!(first.steps[1], second.steps[1], "the step that does");
  }

  #[test]
  fn a_mount_is_not_read_when_no_step_names_where_it_lands() {
    let before = context_with("one");
    let after = context_with("two");

    assert_eq!(
      keys(&with_mount(&before, "apt-get install -y git")).expect("a key"),
      keys(&with_mount(&after, "apt-get install -y git")).expect("a key")
    );
  }

  /// Each mount is keyed on its own contents, and only where a step names it.
  #[test]
  fn keys_every_mount_a_step_reads() {
    let scripts = context_with("one");
    let other = context_with("one");
    let mut plan = with_mount(
      &scripts,
      &format!("cp {MOUNT}/relay /usr/local/bin/ && cp /mnt/extra/x /x"),
    );

    plan.mounts.push(BuildMount {
      destination: "/mnt/extra".to_string(),
      ..mounting(other.path())
    });

    let before = keys(&plan).expect("a key");

    std::fs::write(other.path().join("relay"), "two").expect("a changed file");

    assert_ne!(
      before.steps[1],
      keys(&plan).expect("a key").steps[1],
      "the second mount changed under a step that reads it"
    );
  }

  #[test]
  fn says_where_a_mount_it_cannot_read_was() {
    let mut plan = plan();

    plan.mounts = vec![mounting("/nonexistent/context")];
    plan.steps = vec![BuildStep::root("guest scripts", format!("cp {MOUNT}/x /x"))];

    let error = keys(&plan).expect_err("an unreadable mount");

    assert!(
      error.to_string().contains("/nonexistent/context"),
      "the error should name the mount: {error}"
    );
  }
}
