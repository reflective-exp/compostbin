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

pub use crate::host::agent::{POLL_INTERVAL, serve};
pub use crate::host::ports::{Bound, Forward, PortEvent, bind_all, relay, served};
pub use crate::host::request::{Request, Resolved, resolve};
pub use crate::host::spool::Spool;

/// Fixed, not configurable: the guest client is a shell script and cannot read
/// the manifest.
pub const GUEST_SPOOL_TARGET: &str = "/run/compostbin/host";
/// Where each `<port>.sock` is relayed to; fixed for the same reason.
pub const GUEST_PORTS_TARGET: &str = "/run/compostbin/ports";

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
/// Killed by a signal: this plus the signal, as a shell reports it. Seven bits
/// of signal, so the sum still fits the guest's `exit` byte.
pub const SIGNAL_EXIT_BASE: i32 = 128;

/// Numbered chunks (`<id>.out.000001`, …), not one growing file: a growing file
/// stays stale across the mount, so every inode the guest opens must already be
/// complete.
pub const OUTPUT_STREAM: &str = "out";
pub const ERROR_STREAM: &str = "err";
/// Zero-padded, so the client's glob sorts in sequence order.
pub const SEQUENCE_WIDTH: usize = 6;
/// Big enough that a noisy build does not make thousands of files.
const CHUNK_SIZE: usize = 64 * 1024;
/// A file has no EOF of its own, so the guest marks the end with `<id>.in.eof`.
pub const INPUT_SUFFIX: &str = ".in";
pub const INPUT_EOF_SUFFIX: &str = ".in.eof";
/// Written by the guest before its request when its stdout is a terminal. A
/// `tty = true` command gets a pty only then, so a caller capturing output never
/// gets colour codes and progress redraws.
pub const TTY_SUFFIX: &str = ".tty";
/// Written last, by rename: its appearance means the request is complete.
pub const STATUS_SUFFIX: &str = ".status";
