use compostbin_core::manifest::MANIFEST_RELATIVE_PATH;
use compostbin_core::session::{AddOutcome, CLAUDE_HOME_TARGET, Session};
use compostbin_core::workspace::danger::danger;
use compostbin_core::workspace::paths::PathResolver;
use std::error::Error;
use std::path::Path;

pub fn add(
  resolver: PathResolver,
  project_dir: &Path,
  path: &str,
  force: bool,
  local: bool,
  readonly: bool,
) -> Result<i32, Box<dyn Error>> {
  let canonical = resolver.canonicalize(path)?;

  if let Some(danger) = danger(&canonical, &resolver)
    && !force
  {
    eprintln!("refusing to mount {}: {danger}", canonical.display());
    eprintln!("pass --force if that is really what you want");
    return Ok(1);
  }

  let mut session = Session::load(None, resolver, project_dir)?;

  match session.add(&canonical, readonly, local) {
    AddOutcome::AlreadyMounted { root } => {
      println!("{} is already mounted under {}", canonical.display(), root.display());
      Ok(0)
    }
    AddOutcome::NeedsRestart => {
      session
        .manifest
        .save_to(&project_dir.join(MANIFEST_RELATIVE_PATH), local)?;
      println!(
        "{} recorded; exit the running session and `compostbin run -- --continue` to mount it",
        canonical.display()
      );
      Ok(0)
    }
  }
}

pub fn ls(session: &Session) -> Result<i32, Box<dyn Error>> {
  for entry in session.workspace().entries() {
    let readonly = if entry.readonly { ", readonly" } else { "" };
    println!(
      "{} -> {} ({}{readonly})",
      entry.host.display(),
      entry.guest.display(),
      entry.origin
    );
  }

  println!(
    "{} -> {CLAUDE_HOME_TARGET} (claude home)",
    session.claude_home().display()
  );

  Ok(0)
}
