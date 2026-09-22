//! Rust bindings for Apple's [Containerization] framework: Linux containers,
//! each in its own lightweight VM, in-process and with no daemon.
//!
//! # Requirements
//!
//! macOS 26 on Apple silicon, with Xcode 26 to build. The Swift package is
//! compiled by this crate's build script, which resolves Containerization and
//! its dependencies from the network on a first build.
//!
//! **A binary using this crate must be codesigned with the
//! `com.apple.security.virtualization` entitlement**, or every [`Session::boot`]
//! fails saying so. A plain `cargo build` drops the signature, so re-sign after
//! each one; `containerization.entitlements` in this crate is the file to pass
//! to `codesign --entitlements`.
//!
//! Elsewhere this crate compiles, and every call that needs a VM fails on
//! first use, so a cross-platform workspace still builds.
//!
//! # Shape
//!
//! [`Builder`] turns a [`BuildPlan`] into an image in the [`Store`].
//! [`Session`] boots a [`BootSpec`]'s container from one and runs
//! [`ExecRequest`]s in it. A container belongs to the process that booted it
//! and dies with it; reaching one from elsewhere is the caller's to arrange.
//!
//! [Containerization]: https://github.com/apple/containerization

// `boot` carries a whole `BootSpec` as scalars since no struct crosses the
// bridge, and swift-bridge refuses an `allow` inside its module.
#![cfg_attr(target_os = "macos", allow(clippy::too_many_arguments))]

pub mod error;
pub mod model;
pub mod stdio;
mod store;

#[cfg(target_os = "macos")]
mod bridge;
#[cfg(target_os = "macos")]
mod builder;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod wire;

#[cfg(not(target_os = "macos"))]
mod unsupported;

pub use crate::error::Error;
pub use crate::model::{
  BootSpec, BuildMount, BuildPlan, BuildStep, CachePolicy, Direction, ExecRequest, Mount, Network, Resources, Shell,
  SocketRelay,
};
pub use crate::stdio::{Stdio, UNATTACHED, is_tty, lend};
pub use crate::store::{INITFS_REFERENCE, INITFS_VERSION, KERNEL_VERSION, Store, StoreError};

#[cfg(target_os = "macos")]
pub use crate::builder::Builder;
#[cfg(target_os = "macos")]
pub use crate::session::Session;

#[cfg(not(target_os = "macos"))]
pub use crate::unsupported::{Builder, Session};

/// The bridge's failure code; can't collide with a guest exit code (0...255).
#[cfg(target_os = "macos")]
const FAILED: i32 = -1;

#[cfg(target_os = "macos")]
use crate::bridge::ffi;

/// Turns the bridge's `-1` into the message Swift left behind.
#[cfg(target_os = "macos")]
fn checked(code: i32) -> Result<i32, String> {
  if code == FAILED {
    return Err(ffi::czbridge_last_error());
  }

  Ok(code)
}
