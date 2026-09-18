pub mod cli;
mod report;

use clap::Parser;
use cli::{Arguments, Command};
use compostbin_core::doctor::{self, Status};
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest, SESSIONS_DIR};
use compostbin_core::session::credentials::{KEYCHAIN_SERVICE, Keychain};
use compostbin_core::session::image;
use compostbin_core::session::{AddOutcome, Notice, Session};
use compostbin_core::workspace::Origin;
use compostbin_core::workspace::danger::danger;
use compostbin_core::workspace::paths::PathResolver;
use compostbin_engine::engine::Engine;
use compostbin_engine::model::ExecSpec;
use containerization_framework_bridge::{FrameworkBuilder, FrameworkEngine, Store};
use std::error::Error;
use std::path::Path;

fn report(notice: Notice) {
  match notice {
    Notice::NotInKeychain => eprintln!(
      "no \"{KEYCHAIN_SERVICE}\" entry in the login Keychain; the session will need ANTHROPIC_API_KEY or an interactive login"
    ),
    Notice::Shared(names) => println!("shared from your own ~/.claude: {}", names.join(", ")),
    Notice::Port(event) => eprintln!("compostbin: {event}"),
    Notice::Unpacking(image) => eprintln!("compostbin: unpacking {image}"),
    Notice::SetupFailed { line, code } => eprintln!("compostbin: setup `{line}` exited {code}; not starting Claude"),
    Notice::AgentStopped(error) => eprintln!("compostbin: the host command agent stopped: {error}"),
    Notice::CleanupFailed(error) => eprintln!("compostbin: could not clean up after the session: {error}"),
  }
}

/// Parses argv and executes the requested command, returning its exit code.
pub fn run() -> Result<i32, Box<dyn Error>> {
  let arguments = Arguments::parse();
  let resolver = PathResolver::from_env()?;
  let project_dir = std::env::current_dir()?;
  let manifest_path = project_dir.join(MANIFEST_RELATIVE_PATH);

  match arguments.command {
    Command::Add {
      force,
      local,
      path,
      readonly,
    } => {
      let canonical = resolver.canonicalize(&path.display().to_string())?;

      // A guardrail against slips, not a security boundary (the container has
      // the user's privileges anyway), hence `--force`.
      if let Some(danger) = danger(&canonical, &resolver)
        && !force
      {
        eprintln!("refusing to mount {}: {danger}", canonical.display());
        eprintln!("pass --force if that is really what you want");
        return Ok(1);
      }

      let mut session = load_session(&manifest_path, resolver, &project_dir)?;

      match session.add(&canonical, readonly, local) {
        AddOutcome::AlreadyMounted { root } => {
          println!("{} is already mounted under {}", canonical.display(), root.display());
          Ok(0)
        }
        AddOutcome::NeedsRestart => {
          save_manifest(&session, &manifest_path, local)?;
          println!(
            "{} recorded; exit the running session and `compostbin run -- --continue` to mount it",
            canonical.display()
          );
          Ok(0)
        }
      }
    }

    Command::Build { no_cache } => build_base_image(&load_session(&manifest_path, resolver, &project_dir)?, !no_cache),

    Command::Clean { all } => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      let cleaned = session.clean(all)?;
      let verb = if all { "removed" } else { "cleared" };

      for path in cleaned {
        println!("{verb} {}", path.display());
      }

      Ok(0)
    }

    Command::Doctor => report_diagnosis(&load_session(&manifest_path, resolver, &project_dir)?),

    Command::Init => {
      let mut manifest = Manifest::default();
      manifest.project.name = project_dir
        .file_name()
        .map(|basename| basename.to_string_lossy().into_owned());
      manifest.save(&manifest_path)?;
      println!("wrote {}", manifest_path.display());
      Ok(0)
    }

    Command::Ls => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;

      // Origin matters: in-root vs explicit decides whether `add` needed a
      // restart.
      for entry in session.workspace().entries() {
        let origin = match entry.origin {
          Origin::Explicit => "explicit",
          Origin::Local => "local",
          Origin::Project => "project",
          Origin::Root => "in-root",
        };
        let readonly = if entry.readonly { ", readonly" } else { "" };
        println!(
          "{} -> {} ({origin}{readonly})",
          entry.host.display(),
          entry.guest.display()
        );
      }

      println!(
        "{} -> {} (claude home)",
        session.claude_home().display(),
        compostbin_core::session::CLAUDE_HOME_TARGET
      );

      Ok(0)
    }

    Command::Run { arguments } => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;

      Ok(session.run(&select(&session)?, &Keychain, &arguments, &report)?)
    }

    Command::Shell { user } => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      let spec = ExecSpec {
        user: Some(user),
        ..session.exec_spec(&["bash".to_string()])
      };

      Ok(select(&session)?.exec(&spec)?)
    }
  }
}

/// Builds the base image, and the project's own when the manifest adds to it.
///
/// Provisions the store (kernel and init image) first so there's no separate
/// setup command to remember.
fn build_base_image(session: &Session, cache: bool) -> Result<i32, Box<dyn Error>> {
  println!(
    "building {} into {}",
    session.manifest.project.image,
    image::store(session).display()
  );

  if !session.manifest.image.is_empty() {
    println!("then {}", session.image());
  }

  let builder = FrameworkBuilder::new(Store::at(image::store(session)));

  builder.provision()?;
  image::build(session, &builder, cache)?;

  Ok(0)
}

/// Prints every check; non-zero if any failed, so scripts can gate on it.
fn report_diagnosis(session: &Session) -> Result<i32, Box<dyn Error>> {
  let checks = doctor::diagnose(
    session,
    &select(session)?,
    &Keychain,
    std::env::var_os("ANTHROPIC_API_KEY").is_some(),
  );

  print!("{}", report::Diagnosis(&checks));

  Ok(i32::from(checks.iter().any(|check| check.status == Status::Fail)))
}

/// The engine a session runs on: Containerization.framework, in-process.
/// Nothing else provides images, so the store must be ready first.
fn select(session: &Session) -> Result<FrameworkEngine, Box<dyn Error>> {
  let store = Store::at(image::store(session));

  store.ready()?;

  Ok(FrameworkEngine::new(session.resolve(SESSIONS_DIR), store))
}

/// Writes back only the file the new entry belongs to.
fn save_manifest(session: &Session, manifest_path: &Path, local: bool) -> Result<(), Box<dyn Error>> {
  if local {
    session.manifest.save_local(manifest_path)?;
  } else {
    session.manifest.save(manifest_path)?;
  }

  Ok(())
}

fn load_session(manifest_path: &Path, resolver: PathResolver, project_dir: &Path) -> Result<Session, Box<dyn Error>> {
  Ok(Session::new(Manifest::load(manifest_path)?, resolver, project_dir))
}
