pub mod cli;
mod report;

use clap::Parser;
use cli::{Arguments, Command, Config};
use compostbin_core::doctor::{self, Status};
use compostbin_core::error::At;
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest, PROFILES_DIR, SESSIONS_DIR, TomlFile};
use compostbin_core::session::credentials::{KEYCHAIN_SERVICE, Keychain};
use compostbin_core::session::image;
use compostbin_core::session::settings::HOST_CLAUDE_HOME;
use compostbin_core::session::{AddOutcome, Notice, Process, Session};
use compostbin_core::skills::{self, Action};
use compostbin_core::workspace::Origin;
use compostbin_core::workspace::danger::danger;
use compostbin_core::workspace::paths::PathResolver;
use compostbin_engine::containerization::{self, FrameworkBuilder, FrameworkEngine, StoreError};
use std::error::Error;
use std::io::{IsTerminal, Write};
use std::path::Path;

fn notify(notice: Notice) {
  match notice {
    Notice::NotInKeychain => eprintln!(
      "no \"{KEYCHAIN_SERVICE}\" entry in the login Keychain; the session will need ANTHROPIC_API_KEY or an interactive login"
    ),
    Notice::Shared(names) => println!("shared from your own ~/.claude: {}", names.join(", ")),
    Notice::Port(event) => eprintln!("compostbin: {event}"),
    Notice::Unpacking(image) => eprintln!("compostbin: unpacking {image}"),
    Notice::SetupFailed { line, code } => eprintln!("compostbin: setup `{line}` exited {code}"),
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
      let canonical = resolver.canonicalize(&path)?;

      // A guardrail against slips, not a security boundary (the container has
      // the user's privileges anyway), hence `--force`.
      if let Some(danger) = danger(&canonical, &resolver)
        && !force
      {
        eprintln!("refusing to mount {}: {danger}", canonical.display());
        eprintln!("pass --force if that is really what you want");
        return Ok(1);
      }

      let mut session = Session::new(Manifest::load(&manifest_path)?, resolver, &project_dir);

      match session.add(&canonical, readonly, local) {
        AddOutcome::AlreadyMounted { root } => {
          println!("{} is already mounted under {}", canonical.display(), root.display());
          Ok(0)
        }
        AddOutcome::NeedsRestart => {
          session.manifest.save_to(&manifest_path, local)?;
          println!(
            "{} recorded; exit the running session and `compostbin run -- --continue` to mount it",
            canonical.display()
          );
          Ok(0)
        }
      }
    }

    Command::Build { no_cache, config } => build_base_image(
      &load_session(&manifest_path, resolver, &project_dir, &config)?,
      !no_cache,
    ),

    Command::Clean { all, config } => {
      let session = load_session(&manifest_path, resolver, &project_dir, &config)?;
      let cleaned = session.clean(all)?;
      let verb = if all { "removed" } else { "cleared" };

      for path in cleaned {
        println!("{verb} {}", path.display());
      }

      Ok(0)
    }

    Command::Doctor { config } => report_diagnosis(&load_session(&manifest_path, resolver, &project_dir, &config)?),

    Command::Init => {
      Manifest::named_after(&project_dir).save(&manifest_path)?;
      println!("wrote {}", manifest_path.display());
      Ok(0)
    }

    Command::Install { yes } => install(
      &resolver.resolve(HOST_CLAUDE_HOME).join("skills"),
      &resolver.resolve(PROFILES_DIR),
      yes,
    ),

    Command::Ls { config } => {
      let session = load_session(&manifest_path, resolver, &project_dir, &config)?;

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

    Command::Run { arguments, config } => {
      let session = load_session(&manifest_path, resolver, &project_dir, &config)?;

      Ok(session.run(&select(&session)?, &Keychain, &Process::claude(&arguments), &notify)?)
    }

    Command::Exec {
      argv,
      config,
      tty,
      user,
    } => exec(
      &Process::command(&argv, tty, &user),
      &manifest_path,
      resolver,
      &project_dir,
      &config,
    ),

    Command::Shell { config, user } => exec(
      &Process::command(&["bash".to_string()], true, &user),
      &manifest_path,
      resolver,
      &project_dir,
      &config,
    ),
  }
}

/// Says what a build is about to do, and builds it.
fn build_base_image(session: &Session, cache: bool) -> Result<i32, Box<dyn Error>> {
  println!(
    "building {} into {}",
    session.manifest.project.image,
    image::store(session).display()
  );

  if !session.manifest.image.is_empty() {
    println!("then {}", session.image());
  }

  image::build(
    session,
    &FrameworkBuilder::new(containerization::store(image::store(session))),
    cache,
  )?;

  Ok(0)
}

/// Installs or updates skills into Claude's global skills directory, and
/// creates the empty profiles directory. Prompts for confirmation unless `-y`
fn install(skills_dir: &Path, profiles_dir: &Path, yes: bool) -> Result<i32, Box<dyn Error>> {
  let changes = skills::plan(skills_dir)?;
  let create_profiles = !profiles_dir.is_dir();

  if changes.is_empty() && !create_profiles {
    println!(
      "skills in {} and {} are up to date",
      skills_dir.display(),
      profiles_dir.display()
    );
    return Ok(0);
  }

  if create_profiles {
    println!("create {}/", profiles_dir.display());
  }

  for change in &changes {
    let verb = match change.action {
      Action::Create => "create",
      Action::Update => "update",
      Action::Remove => "remove",
    };
    println!("{verb} {}", change.path.display());
  }

  if !yes {
    if !std::io::stdin().is_terminal() {
      eprintln!("compostbin: pass --yes to install without a terminal to ask on");
      return Ok(1);
    }

    print!("apply? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;

    if !matches!(answer.trim(), "y" | "Y" | "yes") {
      println!("nothing changed");
      return Ok(1);
    }
  }

  skills::apply(&changes)?;
  if create_profiles {
    std::fs::create_dir_all(profiles_dir).at(profiles_dir)?;
  }
  println!("installed");

  Ok(0)
}

/// Runs a command in the session, creating the container if it isn't up.
///
/// A terminal asked for is a terminal required: this side can only pass on one
/// it has, and silently running without would leave whatever wanted it — a
/// shell, anything drawing a UI — with pipes and no way to say so.
fn exec(
  process: &Process,
  manifest_path: &Path,
  resolver: PathResolver,
  project_dir: &Path,
  config: &Config,
) -> Result<i32, Box<dyn Error>> {
  if process.tty && !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
    eprintln!("compostbin: -t needs a terminal on stdin and stdout");
    return Ok(1);
  }

  let session = load_session(manifest_path, resolver, project_dir, config)?;

  Ok(session.run(&select(&session)?, &Keychain, process, &notify)?)
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
  let store = containerization::store(image::store(session));

  store.ready().map_err(|error| match error {
    StoreError::Unreadable { .. } => error.to_string(),
    unbuilt => format!("{unbuilt}; run `compostbin build`"),
  })?;

  Ok(
    FrameworkEngine::new(session.resolver().resolve(SESSIONS_DIR), store)
      .reporting(|error| eprintln!("compostbin: a session client went away: {error}")),
  )
}

/// From the profile when one is named, skipping the project's manifest.
fn load_session(
  manifest_path: &Path,
  resolver: PathResolver,
  project_dir: &Path,
  config: &Config,
) -> Result<Session, Box<dyn Error>> {
  Ok(match &config.profile {
    Some(name) => Session::profiled(name, resolver, project_dir)?,
    None => Session::new(Manifest::load(manifest_path)?, resolver, project_dir),
  })
}
