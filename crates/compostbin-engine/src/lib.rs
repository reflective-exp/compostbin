//! The engine and builder traits, their models, and the build cache keys.
//!
//! Platform-free, so code generic over them (most of `compostbin-core`)
//! compiles anywhere and tests against the fakes here. The implementation is
//! in `containerization-framework-bridge`, macOS only.

pub mod builder;
pub mod cache;
pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
