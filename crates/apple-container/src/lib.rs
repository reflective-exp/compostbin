//! The container a session runs in, and the CLI that still builds its image.

pub mod builder;
pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
