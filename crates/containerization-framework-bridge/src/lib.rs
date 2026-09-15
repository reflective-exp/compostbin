//! Rust's side of the Containerization.framework bridge.
//!
//! The Swift package is only buildable on macOS 26 with Xcode 26. Everywhere
//! else — Linux CI, and a compostbin session's own guest — this crate compiles
//! to nothing, so the workspace still checks from inside a container.

#![cfg(target_os = "macos")]
// A C ABI is wide by nature: `boot` carries a whole `RunSpec` as scalars because
// no struct crosses the bridge. swift-bridge also refuses an `allow` inside its
// own module, so the exemption has to live out here.
#![allow(clippy::too_many_arguments)]

pub mod control;
pub mod engine;
mod spec;
mod store;
mod terminal;

pub use crate::engine::FrameworkEngine;
pub use crate::store::{INITFS_REFERENCE, INITFS_VERSION, Store, StoreError};

use std::fmt;

/// Failure, as the bridge reports it. Never collides with a guest process's own
/// exit code, which is 0...255.
const FAILED: i32 = -1;

#[swift_bridge::bridge]
mod ffi {
  extern "Swift" {
    fn compostbin_last_error() -> String;

    fn compostbin_boot(
      name: &str,
      store_root: &str,
      kernel_path: &str,
      initfs_reference: &str,
      image_reference: &str,
      cpus: i32,
      memory_in_bytes: u64,
      mounts: &str,
      environment: &str,
      arguments: &str,
      working_directory: &str,
      ipv4_address: &str,
      ipv4_gateway: &str,
    ) -> i32;

    fn compostbin_exec(
      name: &str,
      id: &str,
      arguments: &str,
      environment: &str,
      working_directory: &str,
      terminal: i32,
    ) -> i32;

    fn compostbin_resize(id: &str, terminal: i32) -> i32;

    fn compostbin_stop(name: &str) -> i32;

    fn compostbin_is_running(name: &str) -> bool;
  }
}

#[derive(Debug)]
pub struct BridgeError {
  message: String,
}

impl BridgeError {
  fn new(message: impl Into<String>) -> Self {
    Self {
      message: message.into(),
    }
  }
}

impl fmt::Display for BridgeError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}", self.message)
  }
}

impl std::error::Error for BridgeError {}

/// Turns the bridge's `-1` into the message Swift left behind.
fn checked(code: i32) -> Result<i32, BridgeError> {
  if code == FAILED {
    return Err(BridgeError::new(ffi::compostbin_last_error()));
  }

  Ok(code)
}
