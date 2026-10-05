mod cli;
mod engine;
mod install;
mod mount;
mod report;
mod session;

use clap::Parser;
use cli::{Arguments, Command};
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest, TomlFile};
use compostbin_core::session::Process;
use compostbin_core::skills;
use compostbin_core::workspace::paths::PathResolver;
use session::{attach, missing_terminal};
use std::error::Error;

/// Parses argv and executes the requested command, returning its exit code.
pub fn run() -> Result<i32, Box<dyn Error>> {
  let arguments = Arguments::parse();
  let resolver = PathResolver::from_env()?;
  let project_dir = std::env::current_dir()?;

  match arguments.command {
    Command::Add {
      force,
      local,
      path,
      readonly,
    } => mount::add(resolver, &project_dir, &path, force, local, readonly),

    Command::Build { no_cache, config } => engine::build_base_image(&config.load(resolver, &project_dir)?, !no_cache),

    Command::Clean { all, config } => {
      let cleaned = config.load(resolver, &project_dir)?.clean(all)?;
      let verb = if all { "removed" } else { "cleared" };

      for path in cleaned {
        println!("{verb} {}", path.display());
      }

      Ok(0)
    }

    Command::Doctor { config } => report::report_diagnosis(&config.load(resolver, &project_dir)?),

    Command::Init => {
      let manifest_path = project_dir.join(MANIFEST_RELATIVE_PATH);
      Manifest::named_after(&project_dir).save(&manifest_path)?;
      println!("wrote {}", manifest_path.display());
      Ok(0)
    }

    Command::Install { yes } => install::install(&skills::plan(&resolver)?, yes),

    Command::Ls { config } => mount::ls(&config.load(resolver, &project_dir)?),

    Command::Run { arguments, config } => attach(&config.load(resolver, &project_dir)?, &Process::claude(arguments)),

    Command::Exec {
      argv,
      config,
      tty,
      user,
    } => {
      if tty && missing_terminal() {
        return Ok(1);
      }

      attach(
        &config.load(resolver, &project_dir)?,
        &Process::command(argv, tty, user),
      )
    }

    Command::Shell { config, user } => {
      if missing_terminal() {
        return Ok(1);
      }

      attach(
        &config.load(resolver, &project_dir)?,
        &Process::command(vec!["bash".to_string()], true, user),
      )
    }
  }
}
