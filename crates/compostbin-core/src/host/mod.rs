//! The bridge back out of the container: the guest's only way to run anything on
//! the host.
//!
//! The wire format lives here because it is a contract none of its three parties
//! owns: the guest client (a shell script), the spool carrying the files, and the
//! agent serving them. `request` is what may run, `spool` is where the files go,
//! `agent` runs them, `pty` is the terminal a command can ask for.
//!
//! `ports` is the one path that is not files: declared ports, relayed over vmnet.

mod agent;
mod ports;
mod pty;
mod request;
mod spool;

#[cfg(test)]
mod fixtures;

pub use crate::host::agent::{POLL_INTERVAL, serve, serve_once};
pub use crate::host::ports::{Forward, PortEvent, relay};
pub use crate::host::request::{Request, resolve};
pub use crate::host::spool::Spool;

/// Fixed, not configurable: the guest client is a shell script and cannot read
/// the manifest.
pub const GUEST_SPOOL_TARGET: &str = "/run/compostbin/host";

/// Requests land here, one file per request.
pub const REQUESTS_DIR: &str = "requests";
/// Claimed requests are renamed here, so no agent runs one twice.
pub const RUNNING_DIR: &str = "running";
/// Output chunks, and — written last — `<id>.status`.
pub const RESPONSES_DIR: &str = "responses";

/// Submitted as `.partial`, then renamed, so the agent never reads a
/// half-written request.
pub const REQUEST_SUFFIX: &str = ".request";
pub const PARTIAL_SUFFIX: &str = ".partial";

/// The shell's "found but not executable" — the closest existing meaning.
pub const REJECTED_EXIT_CODE: i32 = 126;
/// Killed by a signal. The shell says 128 + signal, but the number is not worth
/// a unix-only import.
pub const SIGNALLED_EXIT_CODE: i32 = 128;

/// Numbered chunks — `<id>.out.000001`, `.000002`, … — not one growing file: a
/// file the guest watches grow stays stale across the mount, so every inode it
/// opens must already be complete.
pub const OUTPUT_STREAM: &str = "out";
pub const ERROR_STREAM: &str = "err";
/// Zero-padded, so a lexicographic sort is sequence order — what the client's
/// glob relies on.
pub const SEQUENCE_WIDTH: usize = 6;
/// Big enough that a noisy build does not make thousands of files.
const CHUNK_SIZE: usize = 64 * 1024;
/// A file has no EOF of its own, so the guest marks the end with `<id>.in.eof`.
pub const INPUT_SUFFIX: &str = ".in";
pub const INPUT_EOF_SUFFIX: &str = ".in.eof";
/// Written by the guest before its request when its own stdout is a terminal. A
/// `tty = true` command only gets a pty when this is present: a caller capturing
/// output would otherwise get colour codes and progress redraws meant for a screen.
pub const TTY_SUFFIX: &str = ".tty";
/// Written last, by rename: its appearance means the request is complete.
pub const STATUS_SUFFIX: &str = ".status";
