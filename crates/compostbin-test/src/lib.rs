#![cfg_attr(feature = "strict", deny(warnings))]
//! Run with `cargo nextest run --features compostbin-test/integration`; its
//! setup script builds the base image the tests boot.

#[cfg(feature = "integration")]
mod lock;
#[cfg(feature = "integration")]
mod output;
#[cfg(feature = "integration")]
mod poll;
#[cfg(feature = "integration")]
mod ports;
#[cfg(feature = "integration")]
mod project;
#[cfg(feature = "integration")]
mod signing;
#[cfg(feature = "integration")]
mod store;
#[cfg(feature = "integration")]
mod terminal;

#[cfg(feature = "integration")]
pub use crate::lock::{Lock, exclusive};
#[cfg(feature = "integration")]
pub use crate::output::{code, stderr, stdout};
#[cfg(feature = "integration")]
pub use crate::poll::poll_until;
#[cfg(feature = "integration")]
pub use crate::ports::unused_port;
#[cfg(feature = "integration")]
pub use crate::project::{Project, Running};
#[cfg(feature = "integration")]
pub use crate::signing::signed_binary;
#[cfg(feature = "integration")]
pub use crate::store::prepare;
#[cfg(feature = "integration")]
pub use crate::terminal::Terminal;
