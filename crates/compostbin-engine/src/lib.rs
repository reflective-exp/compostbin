//! What a session asks of whatever runs containers, and what a build asks of
//! whatever builds images.
//!
//! Traits and models only: no implementation, and nothing platform-specific, so
//! everything generic over them — which is most of `compostbin-core` — stays
//! testable against the fakes here and compiles anywhere. The one implementation
//! lives in `containerization-framework-bridge`, which is macOS-only.

pub mod builder;
pub mod cache;
pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
