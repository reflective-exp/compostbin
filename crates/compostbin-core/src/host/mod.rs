//! The bridge back out of the container: the guest's only way to run anything on
//! the host.
//!
//! The wire format below is a contract shared by the guest client (a shell
//! script), the spool, and the agent. `request` is what may run, `spool` is
//! where the files go, `agent` runs them, `pty` is the terminal a command can
//! ask for. `ports` is the one path that is not files: each declared port is
//! relayed through a unix socket carried into the guest.

mod agent;
mod ports;
mod pty;
mod request;
mod spool;

#[cfg(test)]
mod fixtures;

pub use crate::host::agent::serve;
pub use crate::host::ports::{Bound, Forward, PortEvent, bind_all, relay};
pub use crate::host::spool::Spool;

use crate::manifest::{HostCommand, HostConfig};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// The host command `[host] clipboard` serves, and what the guest's `pbcopy`,
/// `xclip`, `xsel` and `wl-copy` send.
pub const CLIPBOARD_COMMAND: &str = "clipboard";
const CLIPBOARD_ARGV: &[&str] = &["pbcopy"];

/// Fixed, not configurable: the guest client is a shell script and cannot read
/// the manifest.
pub const GUEST_SPOOL_TARGET: &str = "/run/compostbin/host";
/// Where each `<port>.sock` is relayed to; fixed for the same reason.
const GUEST_PORTS_TARGET: &str = "/run/compostbin/ports";

/// Requests land here, one file per request.
const REQUESTS_DIR: &str = "requests";
/// Claimed requests are renamed here, so no agent runs one twice.
const RUNNING_DIR: &str = "running";
/// Output chunks, and — written last — `<id>.status`.
const RESPONSES_DIR: &str = "responses";

/// Submitted as `.partial`, then renamed, so the agent never reads a
/// half-written request.
const REQUEST_SUFFIX: &str = ".request";
const PARTIAL_SUFFIX: &str = ".partial";

/// The shell's "found but not executable" — the closest existing meaning.
pub const REJECTED_EXIT_CODE: i32 = 126;
/// Killed by a signal: this plus the signal, as a shell reports it. Seven bits
/// of signal, so the sum still fits the guest's `exit` byte.
pub const SIGNAL_EXIT_BASE: i32 = 128;

/// Numbered chunks (`<id>.out.000001`, …), not one growing file: a growing file
/// stays stale across the mount, so every inode the guest opens must already be
/// complete.
const OUTPUT_STREAM: &str = "out";
const ERROR_STREAM: &str = "err";
/// Zero-padded, so the client's glob sorts in sequence order.
const SEQUENCE_WIDTH: usize = 6;
/// A file has no EOF of its own, so the guest marks the end with `<id>.in.eof`.
const INPUT_SUFFIX: &str = ".in";
const INPUT_EOF_SUFFIX: &str = ".in.eof";
/// Written by the guest before its request when its stdout is a terminal. A
/// `tty = true` command gets a pty only then, so a caller capturing output never
/// gets colour codes and progress redraws.
const TTY_SUFFIX: &str = ".tty";
/// Written last, by rename: its appearance means the request is complete.
const STATUS_SUFFIX: &str = ".status";

/// What the agent serves: `commands`, plus `clipboard` when it is on. A
/// declared `clipboard` wins, so a project can point it elsewhere.
pub fn served_commands(config: &HostConfig) -> Cow<'_, BTreeMap<String, HostCommand>> {
  if !config.clipboard || config.commands.contains_key(CLIPBOARD_COMMAND) {
    return Cow::Borrowed(&config.commands);
  }

  let mut served = config.commands.clone();
  served.insert(
    CLIPBOARD_COMMAND.to_string(),
    HostCommand {
      arguments: false,
      argv: CLIPBOARD_ARGV.iter().copied().map(str::to_string).collect(),
      deny: Vec::new(),
      tty: false,
    },
  );

  Cow::Owned(served)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn host(text: &str) -> HostConfig {
    toml::from_str(text).expect("host config should parse")
  }

  #[test]
  fn serves_the_clipboard_when_asked() {
    assert_eq!(
      served_commands(&host("clipboard = true\n"))[CLIPBOARD_COMMAND].argv,
      ["pbcopy"]
    );
  }

  #[test]
  fn serves_no_clipboard_unless_asked() {
    assert!(!served_commands(&host("[commands.test]\nargv = [\"true\"]\n")).contains_key(CLIPBOARD_COMMAND));
  }

  #[test]
  fn a_declared_clipboard_command_wins() {
    let config = host("clipboard = true\n[commands.clipboard]\nargv = [\"tee\", \"/tmp/copied\"]\n");

    assert_eq!(served_commands(&config)[CLIPBOARD_COMMAND].argv, ["tee", "/tmp/copied"]);
  }
}
