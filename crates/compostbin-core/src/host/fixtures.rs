//! Allowlist and spool fixtures shared by the submodule tests.

use crate::host::Spool;
use crate::manifest::HostCommand;
use std::collections::BTreeMap;
use tempfile::TempDir;

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
