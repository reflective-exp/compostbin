//! The constants below are a wire format shared with the guest client, a shell
//! script.

mod agent;
mod ports;
mod pty;
mod request;
mod spool;

#[cfg(test)]
mod fixtures;

pub use crate::host::agent::serve;
pub use crate::host::ports::{Bound, Forward, PortEvent, listen_all, relay};
pub use crate::host::spool::Spool;

use crate::manifest::{HostCommand, HostConfig};
use std::borrow::Cow;
use std::collections::BTreeMap;

pub const CLIPBOARD_COMMAND: &str = "clipboard";
const CLIPBOARD_ARGV: &[&str] = &["pbcopy"];

/// Fixed because the guest client cannot read the manifest.
pub const GUEST_SPOOL_TARGET: &str = "/run/compostbin/host";

const REQUESTS_DIR: &str = "requests";
const RUNNING_DIR: &str = "running";
const RESPONSES_DIR: &str = "responses";

/// Written as `.partial` then renamed, so the agent never reads a half-written
/// request.
const REQUEST_SUFFIX: &str = ".request";
const PARTIAL_SUFFIX: &str = ".partial";

pub const REJECTED_EXIT_CODE: i32 = 126;
/// A signal death exits with this plus the signal number, as a shell reports it.
pub const SIGNAL_EXIT_BASE: i32 = 128;

/// Output is written as numbered chunks, not one growing file, because a growing
/// file stays stale across the mount.
const OUTPUT_STREAM: &str = "out";
const ERROR_STREAM: &str = "err";
/// Zero-padded so the client's glob sorts in sequence order.
const SEQUENCE_WIDTH: usize = 6;
/// The guest marks the end of input with `<id>.in.eof`.
const INPUT_SUFFIX: &str = ".in";
const INPUT_EOF_SUFFIX: &str = ".in.eof";
/// Written by the guest when its stdout is a terminal; a `tty = true` command
/// gets a pty only then.
const TTY_SUFFIX: &str = ".tty";
/// Written last, by rename, so its appearance means the response is complete.
const STATUS_SUFFIX: &str = ".status";

/// A declared `clipboard` command overrides the built-in one.
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
