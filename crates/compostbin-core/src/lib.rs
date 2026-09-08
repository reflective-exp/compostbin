#![cfg_attr(feature = "strict", deny(warnings))]

//! Manifest, path resolution, and session lifecycle for compostbin.

pub mod credentials;
pub mod error;
pub mod manifest;
pub mod paths;
pub mod session;
