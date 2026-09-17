//! What is running containers, and whether it holds the image a session needs.

use super::{Check, Status, check};
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::error::EngineError;

pub fn version(engine: &impl Engine) -> Check {
  match engine.version() {
    Ok(Some(version)) => check("engine", Status::Ok, version),
    Ok(None) => check("engine", Status::Warn, "cannot say what version it is"),
    Err(error) => check("engine", Status::Fail, error.to_string()),
  }
}

/// Whether the image store can be read at all.
///
/// There is no daemon to be up or down any more: a session boots from the store
/// on disk, so the question is only whether it is there and readable. It is
/// written by `container build`, which is why the fix is a build.
pub fn store(images: &Result<Vec<String>, EngineError>) -> Check {
  match images {
    Ok(_) => check("image store", Status::Ok, "readable"),
    Err(error) => check("image store", Status::Fail, format!("{error}; run `compostbin build`")),
  }
}

pub fn base_image(session: &Session, images: &Result<Vec<String>, EngineError>) -> Check {
  let wanted = &session.manifest.project.image;

  match images {
    Err(_) => check(
      "base image",
      Status::Fail,
      format!("cannot look for {wanted} while the image store is unreadable"),
    ),
    Ok(images) if images.contains(wanted) => check("base image", Status::Ok, wanted),
    Ok(_) => check(
      "base image",
      Status::Fail,
      format!("{wanted} is not built; run `compostbin build`"),
    ),
  }
}
