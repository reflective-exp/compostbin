#![cfg_attr(feature = "strict", deny(warnings))]
//! Hosts the integration tests in `tests/`, which need a live session on both
//! sides of the mount, and the harness that gives them one.
//!
//! [`Project`] is a throwaway project: its own home, its own manifest, its own
//! container. A test writes the manifest it wants, runs `compostbin exec`, and
//! asserts on what the guest saw. Everything else here exists to make that one
//! call work from a test process:
//!
//! - the binary under test must carry the virtualization entitlement, and a
//!   rebuild drops it, so it is copied aside and signed once per suite
//!   ([`signed_binary`]);
//! - a session reads `HOME` for its state, Claude's home and the host settings
//!   it shares, so each project gets a temporary one and no test can see
//!   another's — or the developer's;
//! - the image store is the one expensive thing, so that temporary home starts
//!   with a copy-on-write clone of one prepared for the suite ([`prepare`]),
//!   and nothing a test writes reaches the developer's.
//!
//! Every test here therefore needs a built base image: `compostbin build` once,
//! then `cargo nextest run --features compostbin-test/integration`, which runs
//! [`prepare`] first.

#[cfg(feature = "integration")]
mod lock;
#[cfg(feature = "integration")]
mod output;
#[cfg(feature = "integration")]
mod poll;
#[cfg(feature = "integration")]
mod ports;
#[cfg(feature = "integration")]
mod project;
#[cfg(feature = "integration")]
mod signing;
#[cfg(feature = "integration")]
mod store;
#[cfg(feature = "integration")]
mod terminal;

#[cfg(feature = "integration")]
pub use crate::lock::{Lock, exclusive};
#[cfg(feature = "integration")]
pub use crate::output::{code, stderr, stdout};
#[cfg(feature = "integration")]
pub use crate::poll::poll_until;
#[cfg(feature = "integration")]
pub use crate::ports::unused_port;
#[cfg(feature = "integration")]
pub use crate::project::{Project, Running};
#[cfg(feature = "integration")]
pub use crate::signing::signed_binary;
#[cfg(feature = "integration")]
pub use crate::store::{REMOVE_AFTER, prepare, remove_after};
#[cfg(feature = "integration")]
pub use crate::terminal::Terminal;
