//! What the container that is running now was actually started with.
//!
//! Mounts cannot be added to a running container, and `run` attaches to a live
//! one rather than recreating it, so a manifest edited mid-session takes effect
//! only on the next create. Without a record that looks like a broken feature.
//!
//! Recorded by whoever creates the container rather than asked of the engine:
//! not every engine can say what a running container was created with.

use crate::manifest::TomlFile;
use compostbin_engine::model::Mount;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Beside Claude's home and the spool, under the session state directory.
pub const RECORD_FILE: &str = "mounts.toml";

/// One mount as it was passed to the engine. Paths are strings: TOML holds
/// them that way, and the record is only ever compared, never resolved.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedMount {
  pub readonly: bool,
  pub source: String,
  pub target: String,
}

impl From<&Mount> for RecordedMount {
  fn from(mount: &Mount) -> Self {
    Self {
      readonly: mount.readonly,
      source: mount.source.display().to_string(),
      target: mount.target.display().to_string(),
    }
  }
}

/// How `doctor` names a mount to the user: source, target, and whether the
/// kernel is holding it read-only.
impl fmt::Display for RecordedMount {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{} -> {}", self.source, self.target)?;

    if self.readonly {
      formatter.write_str(", readonly")?;
    }

    Ok(())
  }
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Record {
  pub mounts: Vec<RecordedMount>,
  /// Not mounts, but fixed at creation in the same way: the guest's relay is
  /// the container's own process, started with them.
  pub ports: Vec<u16>,
}

/// A missing record is not an error: a state directory cleaned mid-session
/// leaves nothing to compare against.
impl TomlFile for Record {}

impl Record {
  pub fn of(mounts: &[Mount], ports: &[u16]) -> Self {
    Self {
      mounts: mounts.iter().map(RecordedMount::from).collect(),
      ports: ports.to_vec(),
    }
  }

  /// How the manifest's mounts and ports differ from the ones the container
  /// really has.
  ///
  /// Mounts are matched by guest path, since that is what a session reaches
  /// for: a mount whose source moved is a *changed* mount rather than a
  /// removal and an add.
  pub fn drift(&self, wanted: &[Mount], ports: &[u16]) -> Drift {
    let mut drift = Drift {
      added_ports: ports
        .iter()
        .filter(|port| !self.ports.contains(port))
        .copied()
        .collect(),
      removed_ports: self
        .ports
        .iter()
        .filter(|port| !ports.contains(port))
        .copied()
        .collect(),
      ..Drift::default()
    };
    let wanted = Self::of(wanted, ports).mounts;

    for mount in &wanted {
      match self
        .mounts
        .iter()
        .find(|recorded| recorded.target == mount.target)
      {
        None => drift.added.push(mount.to_string()),
        Some(recorded) if recorded != mount => drift
          .changed
          .push(format!("{} is {recorded} in the container, not {mount}", mount.target)),
        Some(_) => {}
      }
    }

    for recorded in &self.mounts {
      if !wanted.iter().any(|mount| mount.target == recorded.target) {
        drift.removed.push(recorded.to_string());
      }
    }

    drift
  }
}

/// Every way the manifest and the running container disagree, kept apart because
/// each reads differently: a mount the manifest gained is invisible in the
/// session, one it lost is still exposed, and one that moved points elsewhere.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Drift {
  pub added: Vec<String>,
  pub changed: Vec<String>,
  pub removed: Vec<String>,
  pub added_ports: Vec<u16>,
  pub removed_ports: Vec<u16>,
}

impl Drift {
  pub fn is_empty(&self) -> bool {
    self.added.is_empty()
      && self.changed.is_empty()
      && self.removed.is_empty()
      && self.added_ports.is_empty()
      && self.removed_ports.is_empty()
  }

  /// One line per disagreeing mount or port, phrased by what the user would
  /// otherwise see happen. A line each, because a mount is already two paths
  /// wide.
  pub fn lines(&self) -> Vec<String> {
    let mounts = [
      ("declared but not mounted, so invisible in the session", &self.added),
      ("mounted but no longer declared, so still exposed", &self.removed),
      ("mounted differently", &self.changed),
    ];
    let ports = [
      (
        "declared but not forwarded, so refused in the session",
        &self.added_ports,
      ),
      (
        "forwarded but no longer declared, so still reachable",
        &self.removed_ports,
      ),
    ];

    let mounts = mounts.into_iter().flat_map(|(reason, mounts)| {
      mounts
        .iter()
        .map(move |mount| format!("{mount} — {reason}"))
    });
    let ports = ports.into_iter().flat_map(|(reason, ports)| {
      ports
        .iter()
        .map(move |port| format!("localhost:{port} — {reason}"))
    });

    mounts.chain(ports).collect()
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
    Record::of(mounts, &[])
  }

  /// A port declared after the container started is exactly as unreachable as
  /// a mount added after it started is invisible, and has the same fix.
  #[test]
  fn a_port_declared_since_the_container_started_has_drifted() {
    let drift = Record::of(&[], &[7001]).drift(&[], &[7001, 7002]);

    assert_eq!(drift.added_ports, [7002]);
    assert_eq!(
      drift.lines(),
      ["localhost:7002 — declared but not forwarded, so refused in the session"]
    );
  }

  #[test]
  fn a_port_dropped_from_the_manifest_is_still_reachable() {
    let drift = Record::of(&[], &[7001]).drift(&[], &[]);

    assert_eq!(drift.removed_ports, [7001]);
    assert!(drift.lines()[0].contains("still reachable"), "{drift:?}");
  }

  #[test]
  fn a_recorded_port_that_is_still_declared_has_not_drifted() {
    assert!(Record::of(&[], &[7001]).drift(&[], &[7001]).is_empty());
  }

  #[test]
  fn an_unchanged_manifest_has_not_drifted() {
    let mounts = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    assert!(recorded(&mounts).drift(&mounts, &[]).is_empty());
  }

  #[test]
  fn a_mount_added_since_the_container_started_is_invisible_in_the_session() {
    let started = [mount("/host/a", "/workspace/a")];
    let wanted = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    let drift = recorded(&started).drift(&wanted, &[]);

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

    let drift = recorded(&started).drift(&wanted, &[]);

    assert_eq!(drift.removed, ["/host/secret -> /workspace/secret"]);
    assert!(
      drift.lines().join("\n").contains("still exposed"),
      "an undeclared mount that is still live is the security-relevant half: {drift:?}"
    );
  }

  /// The session still has `/workspace/a`, pointing somewhere else.
  #[test]
  fn a_mount_whose_source_moved_reads_as_one_change() {
    let drift = recorded(&[mount("/host/old", "/workspace/a")]).drift(&[mount("/host/new", "/workspace/a")], &[]);

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

    let drift = recorded(&started).drift(&wanted, &[]);

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

    Record::of(&mounts, &[])
      .save(&path)
      .expect("save should succeed");

    assert_eq!(
      Record::load_if_present(&path).expect("load should succeed"),
      Some(Record::of(&mounts, &[])),
      "the record must survive the process that wrote it"
    );
  }

  /// A session whose state directory was cleaned while it ran. `doctor` says so
  /// rather than claiming a match.
  #[test]
  fn a_missing_record_is_not_an_error() {
    let temp = TempDir::new().expect("temp dir");

    assert_eq!(
      Record::load_if_present(&temp.path().join(RECORD_FILE)).expect("load should succeed"),
      None
    );
  }
}
