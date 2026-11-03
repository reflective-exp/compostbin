use super::{Check, Status, check};
use crate::session::Session;
use compostbin_engine::error::EngineError;
use compostbin_engine::images::Images;
use compostbin_engine::machine::Machine;

pub fn version(machine: &impl Machine) -> Check {
  check("engine", Status::Ok, machine.version())
}

pub fn store(references: Result<&[String], &EngineError>) -> Check {
  match references {
    Ok(_) => check("image store", Status::Ok, "readable"),
    Err(error) => check("image store", Status::Fail, format!("{error}; run `compostbin build`")),
  }
}

pub fn base_image(session: &Session, images: &impl Images, references: Result<&[String], &EngineError>) -> Check {
  let wanted = &session.manifest.project.image;

  match references {
    Err(_) => check(
      "base image",
      Status::Fail,
      format!("cannot look for {wanted} while the image store is unreadable"),
    ),
    Ok(references) if references.contains(wanted) => match images.missing(wanted) {
      Ok(None) => check("base image", Status::Ok, wanted),
      Ok(Some(missing)) => check(
        "base image",
        Status::Fail,
        format!("{wanted} is missing {missing}; run `compostbin build`"),
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
