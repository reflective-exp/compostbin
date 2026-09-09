//! What the container that is running now was actually started with.
//!
//! Mounts cannot be added to a running container, and `start` attaches to a live
//! one rather than recreating it. So a manifest edited mid-session is ignored
//! until `stop` + `run`, which without a record looks like a broken feature.
//!
//! Recorded by whoever creates the container rather than read back out of
//! `container inspect`: that CLI's JSON is Apple's and unversioned, so it would
//! need re-probing on every upgrade, while the mounts we passed are already ours.

use crate::error::{ManifestError, PathError};
use apple_container::model::Mount;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Beside Claude's home and the spool, under the session state directory.
pub const RECORD_FILE: &str = "mounts.toml";

/// One mount as it was passed to `container run`. Paths are strings: TOML holds
/// them that way, and the record is only ever compared, never resolved.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedMount {
  pub readonly: bool,
  pub source: String,
  pub target: String,
}

impl RecordedMount {
  fn of(mount: &Mount) -> Self {
    Self {
      readonly: mount.readonly,
      source: mount.source.display().to_string(),
      target: mount.target.display().to_string(),
    }
  }

  fn describe(&self) -> String {
    let readonly = if self.readonly { ", readonly" } else { "" };

    format!("{} -> {}{readonly}", self.source, self.target)
  }
}

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Record {
  pub mounts: Vec<RecordedMount>,
}

impl Record {
  pub fn of(mounts: &[Mount]) -> Self {
    Self {
      mounts: mounts.iter().map(RecordedMount::of).collect(),
    }
  }

  /// `None` when there is no record, which is not an error: a state directory
  /// cleaned mid-session leaves nothing to compare against.
  pub fn load(path: &Path) -> Result<Option<Self>, ManifestError> {
    let text = match std::fs::read_to_string(path) {
      Ok(text) => text,
      Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(source) => return Err(ManifestError::Io(PathError::new(path, source))),
    };

    toml::from_str(&text)
      .map(Some)
      .map_err(|source| ManifestError::Parse {
        path: path.to_path_buf(),
        source,
      })
  }

  /// Creates the state directory, since `run` may be creating this project's
  /// first container.
  pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
    let rendered = toml::to_string(self).map_err(|source| ManifestError::Render {
      path: path.to_path_buf(),
      source,
    })?;

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|source| ManifestError::Io(PathError::new(parent, source)))?;
    }

    std::fs::write(path, rendered).map_err(|source| ManifestError::Io(PathError::new(path, source)))
  }

  /// How the manifest's mounts differ from the ones the container really has.
  ///
  /// Matched by guest path, since that is what a session reaches for: a mount
  /// whose source moved is a *changed* mount rather than a removal and an add.
  pub fn drift(&self, wanted: &[Mount]) -> Drift {
    let wanted: Vec<RecordedMount> = wanted.iter().map(RecordedMount::of).collect();
    let mut drift = Drift::default();

    for mount in &wanted {
      match self
        .mounts
        .iter()
        .find(|recorded| recorded.target == mount.target)
      {
        None => drift.added.push(mount.describe()),
        Some(recorded) if recorded != mount => drift.changed.push(format!(
          "{} is {} in the container, not {}",
          mount.target,
          recorded.describe(),
          mount.describe()
        )),
        Some(_) => {}
      }
    }

    for recorded in &self.mounts {
      if !wanted.iter().any(|mount| mount.target == recorded.target) {
        drift.removed.push(recorded.describe());
      }
    }

    drift
  }
}

/// Every way the manifest and the running container disagree, kept apart because
/// each reads differently: a mount the manifest gained is invisible in the
/// session, one it lost is still exposed, and one that moved points elsewhere.
#[derive(Debug, Default, PartialEq)]
pub struct Drift {
  pub added: Vec<String>,
  pub changed: Vec<String>,
  pub removed: Vec<String>,
}

impl Drift {
  pub fn is_empty(&self) -> bool {
    self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
  }

  /// One line per disagreeing mount, phrased by what the user would otherwise
  /// see happen. A line each, because a mount is already two paths wide.
  pub fn lines(&self) -> Vec<String> {
    let groups = [
      ("declared but not mounted, so invisible in the session", &self.added),
      ("mounted but no longer declared, so still exposed", &self.removed),
      ("mounted differently", &self.changed),
    ];

    groups
      .iter()
      .flat_map(|(reason, mounts)| {
        mounts
          .iter()
          .map(move |mount| format!("{mount} — {reason}"))
      })
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn mount(source: &str, target: &str) -> Mount {
    Mount {
      readonly: false,
      source: source.into(),
      target: target.into(),
    }
  }

  fn recorded(mounts: &[Mount]) -> Record {
    Record::of(mounts)
  }

  #[test]
  fn an_unchanged_manifest_has_not_drifted() {
    let mounts = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    assert!(recorded(&mounts).drift(&mounts).is_empty());
  }

  #[test]
  fn a_mount_added_since_the_container_started_is_invisible_in_the_session() {
    let started = [mount("/host/a", "/workspace/a")];
    let wanted = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    let drift = recorded(&started).drift(&wanted);

    assert_eq!(drift.added, ["/host/b -> /workspace/b"]);
    assert!(drift.changed.is_empty() && drift.removed.is_empty(), "{drift:?}");
    assert!(
      drift
        .lines()
        .join("\n")
        .contains("invisible in the session"),
      "the message must say what the user would otherwise just see fail: {drift:?}"
    );
  }

  #[test]
  fn a_mount_dropped_from_the_manifest_is_still_exposed() {
    let started = [
      mount("/host/a", "/workspace/a"),
      mount("/host/secret", "/workspace/secret"),
    ];
    let wanted = [mount("/host/a", "/workspace/a")];

    let drift = recorded(&started).drift(&wanted);

    assert_eq!(drift.removed, ["/host/secret -> /workspace/secret"]);
    assert!(
      drift.lines().join("\n").contains("still exposed"),
      "an undeclared mount that is still live is the security-relevant half: {drift:?}"
    );
  }

  /// The session still has `/workspace/a`, pointing somewhere else.
  #[test]
  fn a_mount_whose_source_moved_reads_as_one_change() {
    let drift = recorded(&[mount("/host/old", "/workspace/a")]).drift(&[mount("/host/new", "/workspace/a")]);

    assert!(drift.added.is_empty() && drift.removed.is_empty(), "{drift:?}");
    assert_eq!(drift.changed.len(), 1);
    assert!(
      drift.changed[0].contains("/host/old") && drift.changed[0].contains("/host/new"),
      "the change must name both sides: {drift:?}"
    );
  }

  #[test]
  fn a_mount_that_became_writable_has_drifted() {
    let started = [Mount {
      readonly: true,
      source: "/host/registry".into(),
      target: "/workspace/registry".into(),
    }];
    let wanted = [mount("/host/registry", "/workspace/registry")];

    let drift = recorded(&started).drift(&wanted);

    assert_eq!(
      drift.changed.len(),
      1,
      "`:ro` is kernel-enforced, so it is real: {drift:?}"
    );
    assert!(drift.changed[0].contains("readonly"), "{drift:?}");
  }

  #[test]
  fn round_trips_through_the_state_directory() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("session").join(RECORD_FILE);
    let mounts = [mount("/host/a", "/workspace/a")];

    Record::of(&mounts)
      .save(&path)
      .expect("save should succeed");

    assert_eq!(
      Record::load(&path).expect("load should succeed"),
      Some(Record::of(&mounts)),
      "the record must survive the process that wrote it"
    );
  }

  /// A session whose state directory was cleaned while it ran. `doctor` says so
  /// rather than claiming a match.
  #[test]
  fn a_missing_record_is_not_an_error() {
    let temp = TempDir::new().expect("temp dir");

    assert_eq!(
      Record::load(&temp.path().join(RECORD_FILE)).expect("load should succeed"),
      None
    );
  }
}
