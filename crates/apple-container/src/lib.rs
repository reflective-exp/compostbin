//! Typed wrapper over the Apple `container` CLI.

pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
