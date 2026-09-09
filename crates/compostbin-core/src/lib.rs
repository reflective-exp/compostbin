#![cfg_attr(feature = "strict", deny(warnings))]

//! Manifest, path resolution, and session lifecycle for compostbin.
//!
//! Five domains, each a directory: `manifest` is what the project declares,
//! `session` its container and the state that outlives it, `workspace` which
//! host paths that container can see, `host` the bridge back out of it, and
//! `doctor` what to say when any of them is wrong. `error` stays central because
//! every domain's failures reach the same CLI.

pub mod doctor;
pub mod error;
pub mod host;
pub mod manifest;
pub mod session;
pub mod signals;
pub mod workspace;
