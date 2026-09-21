#![cfg_attr(feature = "strict", deny(warnings))]

//! Manifest, path resolution, and session lifecycle for compostbin.
//!
//! `manifest` is what the project declares, `session` its container and the
//! state that outlives it, `workspace` which host paths that container can see,
//! `host` the bridge back out of it, and `doctor` what to say when any of them
//! is wrong. `error` is shared because every domain's failures reach the same
//! CLI.

pub mod doctor;
pub mod error;
#[cfg(test)]
mod fixtures;
pub mod host;
pub mod manifest;
pub mod session;
pub mod workspace;
