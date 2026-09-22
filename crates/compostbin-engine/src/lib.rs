#![cfg_attr(feature = "strict", deny(warnings))]

//! How compostbin runs containers: the engine and builder traits, their models,
//! the build cache keys, and the one implementation of them.
//!
//! [`containerization`] is that implementation, over the `containerization-framework`
//! crate. The traits stay because `compostbin-core` is written against them and
//! tests against the [`fake`] ones, not because a second engine is coming.

pub mod builder;
pub mod cache;
pub mod containerization;
pub mod engine;
pub mod error;
#[cfg(any(feature = "fake", test))]
pub mod fake;
pub mod model;
