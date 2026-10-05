//! Containerization.framework: building images and selecting the engine.

use compostbin_core::image;
use compostbin_core::session::Session;
use compostbin_engine::containerization::{self, FrameworkBuilder, FrameworkEngine, StoreError};
use std::error::Error;
use std::fmt;

/// Says what a build is about to do, and builds it.
pub fn build_base_image(session: &Session, cache: bool) -> Result<i32, Box<dyn Error>> {
  let store = image::store(session.resolver());

  println!("building {} into {}", session.manifest.project.image, store.display());

  if !session.manifest.image.is_empty() {
    println!("then {}", session.image());
  }

  image::build(session, &FrameworkBuilder::new(containerization::store(store)), cache)?;

  Ok(0)
}

/// A store no session can boot from, and what to do about it.
#[derive(Debug)]
pub struct Unready(StoreError);

impl fmt::Display for Unready {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &self.0 {
      StoreError::Unreadable { .. } => self.0.fmt(formatter),
      unbuilt => write!(formatter, "{unbuilt}; run `compostbin build`"),
    }
  }
}

impl Error for Unready {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    Some(&self.0)
  }
}

/// The engine a session runs on: Containerization.framework, in-process.
/// Nothing else provides images, so the store must be ready first.
pub fn select(session: &Session) -> Result<FrameworkEngine, Unready> {
  let store = containerization::store(image::store(session.resolver()));

  store.ready().map_err(Unready)?;

  Ok(
    FrameworkEngine::new(session.sessions_dir(), store)
      .reporting(|error| eprintln!("compostbin: a session client went away: {error}")),
  )
}
