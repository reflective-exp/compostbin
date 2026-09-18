//! Rust's side of the Containerization.framework bridge.
//!
//! The Swift package builds only on macOS 26 with Xcode 26. Elsewhere the
//! engine and builder are stand-ins that fail on first use, so callers compile
//! unchanged. The store is plain files and works everywhere.

// `boot` carries a whole `RunSpec` as scalars since no struct crosses the
// bridge, and swift-bridge refuses an `allow` inside its module.
#![cfg_attr(target_os = "macos", allow(clippy::too_many_arguments))]

#[cfg(target_os = "macos")]
mod bridge;
#[cfg(target_os = "macos")]
pub mod build;
#[cfg(target_os = "macos")]
pub mod control;
#[cfg(target_os = "macos")]
pub mod engine;
#[cfg(target_os = "macos")]
mod spec;
mod store;
#[cfg(target_os = "macos")]
mod terminal;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use crate::build::FrameworkBuilder;
#[cfg(target_os = "macos")]
pub use crate::engine::FrameworkEngine;
pub use crate::store::{INITFS_REFERENCE, INITFS_VERSION, KERNEL_VERSION, Store, StoreError};
#[cfg(not(target_os = "macos"))]
pub use crate::unsupported::{FrameworkBuilder, FrameworkEngine};

/// The bridge's failure code; can't collide with a guest exit code (0...255).
#[cfg(target_os = "macos")]
const FAILED: i32 = -1;

#[cfg(target_os = "macos")]
use crate::bridge::ffi;

#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct BridgeError {
  message: String,
}

#[cfg(target_os = "macos")]
impl BridgeError {
  fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

#[cfg(target_os = "macos")]
impl std::fmt::Display for BridgeError {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "{}", self.message)
  }
}

#[cfg(target_os = "macos")]
impl std::error::Error for BridgeError {}

/// Turns the bridge's `-1` into the message Swift left behind.
#[cfg(target_os = "macos")]
fn checked(code: i32) -> Result<i32, BridgeError> {
  if code == FAILED {
    return Err(BridgeError::new(ffi::compostbin_last_error()));
  }

  Ok(code)
}
