//! Allowlist and spool fixtures shared by the submodule tests, and the guest
//! client's half of the wire format.

use crate::error::PathError;
use crate::host::REQUEST_SUFFIX;
use crate::host::request::Request;
use crate::host::spool::{Spool, publish};
use crate::manifest::HostCommand;
use std::collections::BTreeMap;
use tempfile::TempDir;

impl Request {
  pub fn new(command: impl Into<String>, arguments: Vec<String>) -> Self {
    Self {
      arguments,
      command: command.into(),
    }
  }

  /// What the guest client writes: one field per line, command first. The
  /// client refuses a newline in a field, so a fixture must not contain one.
  pub fn render(&self) -> String {
    let fields: Vec<&str> = std::iter::once(&self.command)
      .chain(&self.arguments)
      .map(String::as_str)
      .collect();
    assert!(
      fields.iter().all(|field| !field.contains('\n')),
      "the guest client never sends a newline in a field: {fields:?}"
    );

    fields.iter().map(|field| format!("{field}\n")).collect()
  }
}

impl Spool {
  /// Writes a request the way the guest does: `.partial` first, then rename.
  pub fn submit(&self, id: &str, request: &Request) -> Result<(), PathError> {
    publish(&self.requests(), &format!("{id}{REQUEST_SUFFIX}"), request.render())
  }
}

pub fn commands(entries: &[(&str, &[&str], bool)]) -> BTreeMap<String, HostCommand> {
  entries
    .iter()
    .map(|(name, argv, arguments)| {
      (
        name.to_string(),
        HostCommand {
          arguments: *arguments,
          argv: argv.iter().copied().map(str::to_string).collect(),
          deny: Vec::new(),
          // Pipes keep the streams separate, so each can be asserted on.
          tty: false,
        },
      )
    })
    .collect()
}

/// One command declared `tty = true`, the only way to reach the pty path.
pub fn terminal_command(name: &str, argv: &[&str]) -> BTreeMap<String, HostCommand> {
  let mut commands = commands(&[(name, argv, false)]);
  commands
    .get_mut(name)
    .expect("the command was just inserted")
    .tty = true;
  commands
}

/// `test-one` denies what a cargo project would.
pub fn allowlist() -> BTreeMap<String, HostCommand> {
  let mut commands = commands(&[
    ("test", &["cargo", "nextest", "run", "--workspace"], false),
    ("test-one", &["cargo", "nextest", "run"], true),
  ]);
  commands
    .get_mut("test-one")
    .expect("the command was just inserted")
    .deny = ["--config", "--manifest-path", "-Z"]
    .iter()
    .copied()
    .map(str::to_string)
    .collect();
  commands
}

/// A spool with its directories already created, as the host makes it.
pub fn spool() -> (TempDir, Spool) {
  let temp = TempDir::new().expect("temp dir");
  let spool = Spool::new(temp.path().canonicalize().expect("canonical temp"));
  spool.create().expect("create spool");
  (temp, spool)
}
