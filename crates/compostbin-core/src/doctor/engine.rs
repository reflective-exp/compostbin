//! What is running containers, and whether it holds the image a session needs.

use super::{Check, Status, check};
use crate::session::Session;
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;

pub fn version(engine: &impl Engine) -> Check {
  check("engine", Status::Ok, engine.version())
}

/// Whether the image store can be read at all.
///
/// There is no daemon to be up or down: a session boots from the store on disk,
/// which `compostbin build` writes.
pub fn store(images: Result<&[String], &EngineError>) -> Check {
  match images {
    Ok(_) => check("image store", Status::Ok, "readable"),
    Err(error) => check("image store", Status::Fail, format!("{error}; run `compostbin build`")),
  }
}

pub fn base_image(session: &Session, engine: &impl Engine, images: Result<&[String], &EngineError>) -> Check {
  let wanted = &session.manifest.project.image;

  match images {
    Err(_) => check(
      "base image",
      Status::Fail,
      format!("cannot look for {wanted} while the image store is unreadable"),
    ),
    Ok(images) if images.contains(wanted) => match engine.missing_content(wanted) {
      Ok(None) => check("base image", Status::Ok, wanted),
      Ok(Some(digest)) => check(
        "base image",
        Status::Fail,
        format!("{wanted} is missing blob {digest}; run `compostbin build`"),
      ),
      Err(error) => check("base image", Status::Fail, error.to_string()),
    },
    Ok(_) => check(
      "base image",
      Status::Fail,
      format!("{wanted} is not built; run `compostbin build`"),
    ),
  }
}
