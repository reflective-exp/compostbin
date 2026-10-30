//! Written at creation because not every engine can report what a running container was created with.

use crate::manifest::{HostConfig, HostSocket, TomlFile};
use compostbin_engine::model::Mount;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

pub const RECORD_FILE: &str = "mounts.toml";

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
  pub ports: Vec<u16>,
  /// As declared, before `~` is expanded.
  pub sockets: BTreeMap<String, HostSocket>,
}

/// A socket as `doctor` names it.
fn describe(name: &str, socket: &HostSocket) -> String {
  format!(
    "{} → {}, mode {}",
    socket.target(name).display(),
    socket.source,
    socket.mode
  )
}

impl TomlFile for Record {}

impl Record {
  pub fn of(mounts: &[Mount], host: &HostConfig) -> Self {
    Self {
      mounts: mounts.iter().map(RecordedMount::from).collect(),
      ports: host.ports.clone(),
      sockets: host.sockets.clone(),
    }
  }

  /// Mounts match by guest path, so a moved source is a change rather than a removal and an add.
  /// Sockets match by name, for the same reason.
  pub fn drift(&self, wanted: &[Mount], host: &HostConfig) -> Drift {
    let ports = &host.ports;
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

    for (name, socket) in &host.sockets {
      match self.sockets.get(name) {
        None => drift.added_sockets.push(describe(name, socket)),
        Some(recorded) if recorded != socket => drift.changed_sockets.push(format!(
          "socket `{name}` is {} in the container, not {}",
          describe(name, recorded),
          describe(name, socket)
        )),
        Some(_) => {}
      }
    }
    drift.removed_sockets = self
      .sockets
      .iter()
      .filter(|(name, _)| !host.sockets.contains_key(*name))
      .map(|(name, socket)| describe(name, socket))
      .collect();

    let wanted = Self::of(wanted, host).mounts;

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

#[derive(Debug, Default, Eq, PartialEq)]
pub struct Drift {
  pub added: Vec<String>,
  pub changed: Vec<String>,
  pub removed: Vec<String>,
  pub added_ports: Vec<u16>,
  pub removed_ports: Vec<u16>,
  /// Sockets as `describe` names them.
  pub added_sockets: Vec<String>,
  pub removed_sockets: Vec<String>,
  pub changed_sockets: Vec<String>,
}

impl Drift {
  pub fn is_empty(&self) -> bool {
    self.added.is_empty()
      && self.changed.is_empty()
      && self.removed.is_empty()
      && self.added_ports.is_empty()
      && self.removed_ports.is_empty()
      && self.added_sockets.is_empty()
      && self.removed_sockets.is_empty()
      && self.changed_sockets.is_empty()
  }

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
    let sockets = [
      (
        "declared but not forwarded, so missing in the session",
        &self.added_sockets,
      ),
      (
        "forwarded but no longer declared, so still reachable",
        &self.removed_sockets,
      ),
    ];
    let sockets = sockets.into_iter().flat_map(|(reason, sockets)| {
      sockets
        .iter()
        .map(move |socket| format!("{socket} — {reason}"))
    });

    mounts
      .chain(ports)
      .chain(sockets)
      .chain(self.changed_sockets.iter().cloned())
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

  fn ports(ports: &[u16]) -> HostConfig {
    HostConfig {
      ports: ports.to_vec(),
      ..HostConfig::default()
    }
  }

  fn sockets(sockets: &[(&str, &str)]) -> HostConfig {
    HostConfig {
      sockets: sockets
        .iter()
        .map(|(name, source)| (name.to_string(), HostSocket::new(*source)))
        .collect(),
      ..HostConfig::default()
    }
  }

  fn recorded(mounts: &[Mount]) -> Record {
    Record::of(mounts, &HostConfig::default())
  }

  #[test]
  fn a_socket_declared_since_the_container_started_has_drifted() {
    let drift =
      Record::of(&[], &sockets(&[("a", "/a.sock")])).drift(&[], &sockets(&[("a", "/a.sock"), ("b", "/b.sock")]));

    assert_eq!(drift.added_sockets, ["/run/compostbin/sockets/b → /b.sock, mode 0660"]);
    assert_eq!(
      drift.lines(),
      ["/run/compostbin/sockets/b → /b.sock, mode 0660 — declared but not forwarded, so missing in the session"]
    );
  }

  #[test]
  fn a_socket_dropped_from_the_manifest_is_still_reachable() {
    let drift = Record::of(&[], &sockets(&[("a", "/a.sock")])).drift(&[], &HostConfig::default());

    assert_eq!(
      drift.removed_sockets,
      ["/run/compostbin/sockets/a → /a.sock, mode 0660"]
    );
    assert!(drift.lines()[0].contains("still reachable"), "{drift:?}");
  }

  #[test]
  fn a_socket_changed_in_any_way_reads_as_one_change() {
    let recorded = Record::of(&[], &sockets(&[("a", "/old.sock")]));
    let mut moved = sockets(&[("a", "/old.sock")]);
    moved.sockets.get_mut("a").expect("a socket").target = Some("/run/a.sock".into());

    for wanted in [sockets(&[("a", "/new.sock")]), moved] {
      let drift = recorded.drift(&[], &wanted);

      assert!(
        drift.added_sockets.is_empty() && drift.removed_sockets.is_empty(),
        "{drift:?}"
      );
      assert_eq!(drift.changed_sockets.len(), 1, "{drift:?}");
    }

    assert_eq!(
      recorded.drift(&[], &sockets(&[("a", "/new.sock")])).lines(),
      [
        "socket `a` is /run/compostbin/sockets/a → /old.sock, mode 0660 in the container, not /run/compostbin/sockets/a → /new.sock, mode 0660"
      ]
    );
  }

  #[test]
  fn a_record_from_before_sockets_reads_as_none() {
    let record: Record = toml::from_str("mounts = []\nports = [7001]\n").expect("an old record should parse");

    assert!(record.drift(&[], &ports(&[7001])).is_empty());
  }

  #[test]
  fn a_port_declared_since_the_container_started_has_drifted() {
    let drift = Record::of(&[], &ports(&[7001])).drift(&[], &ports(&[7001, 7002]));

    assert_eq!(drift.added_ports, [7002]);
    assert_eq!(
      drift.lines(),
      ["localhost:7002 — declared but not forwarded, so refused in the session"]
    );
  }

  #[test]
  fn a_port_dropped_from_the_manifest_is_still_reachable() {
    let drift = Record::of(&[], &ports(&[7001])).drift(&[], &HostConfig::default());

    assert_eq!(drift.removed_ports, [7001]);
    assert!(drift.lines()[0].contains("still reachable"), "{drift:?}");
  }

  #[test]
  fn a_recorded_port_that_is_still_declared_has_not_drifted() {
    assert!(
      Record::of(&[], &ports(&[7001]))
        .drift(&[], &ports(&[7001]))
        .is_empty()
    );
  }

  #[test]
  fn an_unchanged_manifest_has_not_drifted() {
    let mounts = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    assert!(
      recorded(&mounts)
        .drift(&mounts, &HostConfig::default())
        .is_empty()
    );
  }

  #[test]
  fn a_mount_added_since_the_container_started_is_invisible_in_the_session() {
    let started = [mount("/host/a", "/workspace/a")];
    let wanted = [mount("/host/a", "/workspace/a"), mount("/host/b", "/workspace/b")];

    let drift = recorded(&started).drift(&wanted, &HostConfig::default());

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

    let drift = recorded(&started).drift(&wanted, &HostConfig::default());

    assert_eq!(drift.removed, ["/host/secret -> /workspace/secret"]);
    assert!(
      drift.lines().join("\n").contains("still exposed"),
      "an undeclared mount that is still live is the security-relevant half: {drift:?}"
    );
  }

  #[test]
  fn a_mount_whose_source_moved_reads_as_one_change() {
    let drift = recorded(&[mount("/host/old", "/workspace/a")])
      .drift(&[mount("/host/new", "/workspace/a")], &HostConfig::default());

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

    let drift = recorded(&started).drift(&wanted, &HostConfig::default());

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

    Record::of(&mounts, &sockets(&[("mcp", "~/mcp.sock")]))
      .save(&path)
      .expect("save should succeed");

    assert_eq!(
      Record::load_if_present(&path).expect("load should succeed"),
      Some(Record::of(&mounts, &sockets(&[("mcp", "~/mcp.sock")]))),
      "the record must survive the process that wrote it"
    );
  }

  #[test]
  fn a_missing_record_is_not_an_error() {
    let temp = TempDir::new().expect("temp dir");

    assert_eq!(
      Record::load_if_present(&temp.path().join(RECORD_FILE)).expect("load should succeed"),
      None
    );
  }
}
