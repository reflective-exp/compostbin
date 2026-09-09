#![cfg_attr(feature = "strict", deny(warnings))]

//! Manifest, path resolution, and session lifecycle for compostbin.

pub mod credentials;
pub mod doctor;
pub mod error;
pub mod host;
pub mod image;
pub mod manifest;
pub mod paths;
pub mod session;
pub mod signals;
pub mod workspace;
