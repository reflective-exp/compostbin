use compostbin_core::image;
use compostbin_core::session::Session;
use compostbin_engine::containerization::{FrameworkBuilder, FrameworkEngine, Store, StoreError, describe, sweep};
use std::error::Error;
use std::fmt;

pub fn build_base_image(session: &Session, cache: bool) -> Result<i32, Box<dyn Error>> {
  let store = image::store(session.resolver());

  println!("building {} into {}", session.manifest.project.image, store.display());

  if !session.manifest.image.is_empty() {
    println!("then {}", session.image());
  }

  image::build(session, &FrameworkBuilder::new(Store::at(store)), cache)?;

  Ok(0)
}

pub fn clean(session: &Session, all: bool) -> Result<i32, Box<dyn Error>> {
  // Before `session.clean`, which with `all` takes the control socket the sweep checks.
  let swept = sweep(&Store::at(image::store(session.resolver())), &session.sessions_dir());

  for path in swept.removed {
    println!("removed {}", path.display());
  }

  if let Some(blobs) = &swept.blobs
    && let Some(description) = describe(blobs)
  {
    if blobs.is_err() {
      eprintln!("compostbin: {description}");
    } else {
      println!("{description}");
    }
  }

  let verb = if all { "removed" } else { "cleared" };

  for path in session.clean(all)? {
    println!("{verb} {}", path.display());
  }

  Ok(0)
}

#[derive(Debug)]
pub struct Unready(StoreError);

impl fmt::Display for Unready {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}; run `compostbin build`", self.0)
  }
}

impl Error for Unready {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    Some(&self.0)
  }
}

pub fn select(session: &Session) -> Result<FrameworkEngine, Unready> {
  let store = Store::at(image::store(session.resolver()));

  store.ready().map_err(Unready)?;

  Ok(
    FrameworkEngine::new(session.sessions_dir(), store)
      .reporting(|error| eprintln!("compostbin: a session client went away: {error}")),
  )
}
