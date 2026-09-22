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
//! - the image store is the one expensive thing, so that temporary home
//!   borrows the real one rather than building images again.
//!
//! Every test here therefore needs a built base image: `compostbin build` once,
//! then `cargo nextest run --features compostbin-test/integration`.

#[cfg(feature = "integration")]
mod project;

#[cfg(feature = "integration")]
pub use crate::project::{Lock, Project, Running, code, exclusive, signed_binary, stderr, stdout};
