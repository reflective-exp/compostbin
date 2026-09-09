//! What the `container` CLI and its daemon say about themselves, and whether
//! they hold the image a session needs.

use super::{Check, Status, check};
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::error::EngineError;

/// The `container` CLI version this crate's behaviour was verified against.
pub const TESTED_CLI_VERSION: &str = "1.3.1";

pub fn cli_version(engine: &impl Engine) -> Check {
  match engine.version() {
    Ok(Some(version)) if version == TESTED_CLI_VERSION => check("container CLI", Status::Ok, version),
    Ok(Some(version)) => check(
      "container CLI",
      Status::Warn,
      format!("{version}; compostbin was verified against {TESTED_CLI_VERSION}"),
    ),
    Ok(None) => check("container CLI", Status::Warn, "unrecognised version output"),
    Err(error) => check("container CLI", Status::Fail, error.to_string()),
  }
}

pub fn daemon(images: &Result<Vec<String>, EngineError>) -> Check {
  match images {
    Ok(_) => check("daemon", Status::Ok, "responding"),
    Err(error) => check("daemon", Status::Fail, format!("{error}; run `container system start`")),
  }
}

pub fn base_image(session: &Session, images: &Result<Vec<String>, EngineError>) -> Check {
  let wanted = &session.manifest.project.image;

  match images {
    Err(_) => check(
      "base image",
      Status::Fail,
      format!("cannot look for {wanted} while the daemon is unreachable"),
    ),
    Ok(images) if images.contains(wanted) => check("base image", Status::Ok, wanted),
    Ok(_) => check(
      "base image",
      Status::Fail,
      format!("{wanted} is not built; run `compostbin build`"),
    ),
  }
}
