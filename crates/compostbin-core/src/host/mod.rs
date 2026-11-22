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
pub use crate::host::ports::{
  Bound, Forward, GUEST_SOCKETS_DIR, GUEST_SOCKETS_GROUP, PortEvent, Upstream, listen_all, relay,
};
pub use crate::host::spool::Spool;

use crate::manifest::{HostCommand, HostConfig};
use std::borrow::Cow;
use std::collections::BTreeMap;

pub const CLIPBOARD_COMMAND: &str = "clipboard";
const CLIPBOARD_ARGV: &[&str] = &["pbcopy"];
pub const CLIPBOARD_IMAGE_COMMAND: &str = "clipboard-image";
/// Writes the pasteboard's PNG, or its TIFF converted to PNG, to stdout, and fails on anything else.
const CLIPBOARD_IMAGE_ARGV: &[&str] = &[
  "osascript",
  "-l",
  "JavaScript",
  "-e",
  "ObjC.import(\"AppKit\"); \
   var board = $.NSPasteboard.generalPasteboard; \
   var png = board.dataForType(\"public.png\"); \
   if (png.isNil()) { \
   var tiff = board.dataForType(\"public.tiff\"); \
   if (tiff.isNil()) throw new Error(\"the clipboard holds no image\"); \
   png = $.NSBitmapImageRep.imageRepWithData(tiff).representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $({})); \
   } \
   $.NSFileHandle.fileHandleWithStandardOutput.writeData(png);",
];

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

/// A declared command overrides the built-in one of the same name.
pub fn served_commands(config: &HostConfig) -> Cow<'_, BTreeMap<String, HostCommand>> {
  if !config.clipboard {
    return Cow::Borrowed(&config.commands);
  }

  let mut served = config.commands.clone();
  for (name, argv) in [
    (CLIPBOARD_COMMAND, CLIPBOARD_ARGV),
    (CLIPBOARD_IMAGE_COMMAND, CLIPBOARD_IMAGE_ARGV),
  ] {
    served
      .entry(name.to_string())
      .or_insert_with(|| HostCommand {
        arguments: false,
        argv: argv.iter().copied().map(str::to_string).collect(),
        deny: Vec::new(),
        tty: false,
      });
  }

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
    let config = host("clipboard = true\n");
    let served = served_commands(&config);

    assert_eq!(served[CLIPBOARD_COMMAND].argv, ["pbcopy"]);
    assert_eq!(served[CLIPBOARD_IMAGE_COMMAND].argv[0], "osascript");
    assert!(
      !served.values().any(|command| command.arguments),
      "the guest cannot pass arguments to the clipboard commands"
    );
  }

  #[test]
  fn serves_no_clipboard_unless_asked() {
    let config = host("[commands.test]\nargv = [\"true\"]\n");
    let served = served_commands(&config);

    assert!(!served.contains_key(CLIPBOARD_COMMAND));
    assert!(!served.contains_key(CLIPBOARD_IMAGE_COMMAND));
  }

  #[test]
  fn a_declared_clipboard_command_wins() {
    let config = host("clipboard = true\n[commands.clipboard]\nargv = [\"tee\", \"/tmp/copied\"]\n");
    let served = served_commands(&config);

    assert_eq!(served[CLIPBOARD_COMMAND].argv, ["tee", "/tmp/copied"]);
    assert_eq!(
      served[CLIPBOARD_IMAGE_COMMAND].argv[0], "osascript",
      "replacing one does not drop the other"
    );
  }

  #[test]
  fn a_declared_clipboard_image_command_wins() {
    let config = host("clipboard = true\n[commands.clipboard-image]\nargv = [\"cat\", \"/tmp/image.png\"]\n");

    assert_eq!(
      served_commands(&config)[CLIPBOARD_IMAGE_COMMAND].argv,
      ["cat", "/tmp/image.png"]
    );
  }
}
