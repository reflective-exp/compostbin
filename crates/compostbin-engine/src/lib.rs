//! What a session asks of whatever runs containers, and what a build asks of
//! whatever builds images.
//!
//! Traits and models, plus what any builder would compute the same way — the
//! build cache keys. Nothing platform-specific, so everything generic over them
//! — which is most of `compostbin-core` — stays testable against the fakes here
//! and compiles anywhere. The one implementation lives in
//! `containerization-framework-bridge`, and runs only on macOS.

pub mod builder;
pub mod cache;
pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
