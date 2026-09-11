pub mod cli;
mod report;

use apple_container::engine::{CliEngine, Engine};
use clap::Parser;
use cli::{Arguments, Command};
use compostbin_core::doctor::{self, Status};
use compostbin_core::host::{self, Forward, PortEvent, Spool};
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest};
use compostbin_core::session::credentials::{self, Keychain, SeedOutcome};
use compostbin_core::session::image;
use compostbin_core::session::settings;
use compostbin_core::session::{AddOutcome, Session};
use compostbin_core::signals;
use compostbin_core::workspace::Origin;
use compostbin_core::workspace::danger::danger;
use compostbin_core::workspace::paths::PathResolver;
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// The session's declared ports, relayed from its container's gateway. Empty,
/// after saying why, when there is no gateway to listen on: the session still
/// runs, it just forwards nothing.
fn port_forwards(session: &Session, engine: &impl Engine) -> Vec<Forward> {
  if !session.manifest.host.has_ports() {
    return Vec::new();
  }

  match engine.gateway(&session.container_name()) {
    Ok(Some(gateway)) => session.forwards(gateway),
    Ok(None) => {
      eprintln!(
        "compostbin: `container` reports no gateway for {}; host ports are not forwarded",
        session.container_name()
      );
      Vec::new()
    }
    Err(error) => {
      eprintln!("compostbin: finding the gateway: {error}; host ports are not forwarded");
      Vec::new()
    }
  }
}

/// What the relay says, in the terms a user would look for.
fn report_port(event: PortEvent) {
  match event {
    PortEvent::Listening(forward) => {
      eprintln!("compostbin: forwarding {} to {}", forward.listen, forward.upstream)
    }
    PortEvent::Deferred(forward) => eprintln!(
      "compostbin: another session already forwards {}, which serves this one too; taking over when it exits",
      forward.listen
    ),
    PortEvent::Unbindable(forward, error) => {
      eprintln!("compostbin: cannot listen on {}: {error}; retrying", forward.listen)
    }
    PortEvent::UpstreamRefused(forward, error) => {
      eprintln!("compostbin: nothing answers at {}: {error}", forward.upstream)
    }
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
      path,
      readonly,
      restart,
    } => {
      let canonical = resolver.canonicalize(&path.display().to_string())?;

      // A guardrail against a slip rather than a boundary — the container runs
      // with the user's own privileges either way — hence `--force`.
      if let Some(danger) = danger(&canonical, &resolver)
        && !force
      {
        eprintln!("refusing to mount {}: {danger}", canonical.display());
        eprintln!("pass --force if that is really what you want");
        return Ok(1);
      }

      let mut session = load_session(&manifest_path, resolver, &project_dir)?;

      match session.add(&canonical, readonly) {
        AddOutcome::AlreadyMounted { root } => {
          session.manifest.save(&manifest_path)?;
          println!("{} is already mounted under {}", canonical.display(), root.display());
          Ok(0)
        }
        AddOutcome::NeedsRestart if restart => {
          session.manifest.save(&manifest_path)?;
          Ok(session.restart(&CliEngine::new())?)
        }
        AddOutcome::NeedsRestart => {
          session.manifest.save(&manifest_path)?;
          println!(
            "{} recorded; run `compostbin add --restart` or restart the session to mount it",
            canonical.display()
          );
          Ok(0)
        }
      }
    }

    Command::Build => build_base_image(&load_session(&manifest_path, resolver, &project_dir)?),

    Command::Clean { all } => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;

      for removed in session.clean(all)? {
        println!("removed {}", removed.display());
      }

      Ok(0)
    }

    Command::Doctor => report_diagnosis(&load_session(&manifest_path, resolver, &project_dir)?),

    Command::HostAgent => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      session.prepare_host_spool()?;

      if session.manifest.host.is_empty() {
        eprintln!(
          "no [host.commands] or [host] ports in {}; there is nothing to serve",
          manifest_path.display()
        );
        return Ok(1);
      }

      let forwards = port_forwards(&session, &CliEngine::new());
      if forwards.is_empty() && !session.manifest.host.has_commands() {
        return Ok(1);
      }

      // Before the loop, so a signal arriving immediately stops the agent rather
      // than killing a claimed request.
      let stop = signals::stop_on_termination()?;

      if session.manifest.host.has_commands() {
        println!(
          "serving {} host commands from {}",
          session.manifest.host.commands.len(),
          session.host_spool().display()
        );
      }
      println!("SIGINT or SIGTERM stops it, a second one kills it");

      std::thread::scope(|scope| {
        if !forwards.is_empty() {
          scope.spawn(|| host::relay(&forwards, stop, &report_port));
        }

        let served = if session.manifest.host.has_commands() {
          host::serve(
            &Spool::new(session.host_spool()),
            &session.manifest.host.commands,
            &project_dir,
            session.manifest.host.concurrency,
            stop,
          )
        } else {
          while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(host::POLL_INTERVAL);
          }
          Ok(())
        };

        // A failed agent must not leave the relay holding the scope open.
        stop.store(true, Ordering::Relaxed);
        served
      })?;

      println!("stopped");

      Ok(0)
    }

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

      // Host path, then where it appears in the container, then why: in-root
      // rather than explicit is what decides whether `add` needed a restart.
      for entry in session.workspace().entries() {
        let origin = match entry.origin {
          Origin::Explicit => "explicit",
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

      if credentials::seed(
        &session.claude_home(),
        session.manifest.claude.seed_from_keychain,
        &Keychain,
      )? == SeedOutcome::NotInKeychain
      {
        eprintln!(
          "no \"{}\" entry in the login Keychain; the session will need ANTHROPIC_API_KEY or an interactive login",
          credentials::KEYCHAIN_SERVICE
        );
      }

      let shared = settings::share(
        &session.resolve(settings::HOST_CLAUDE_HOME),
        &session.claude_home(),
        &session.manifest.claude.shared,
      )?;
      if !shared.is_empty() {
        println!("shared from your own ~/.claude: {}", shared.join(", "));
      }

      let engine = CliEngine::new();
      session.prepare_host_spool()?;
      session.start(&engine)?;

      let forwards = port_forwards(&session, &engine);
      if let Some(forward) = forwards.first() {
        let ports: Vec<String> = session
          .manifest
          .host
          .ports
          .iter()
          .map(u16::to_string)
          .collect();
        println!(
          "forwarding host ports {} via {}, where every container on vmnet can reach them",
          ports.join(", "),
          forward.listen.ip()
        );
      }

      let mut claude = vec!["claude".to_string()];
      claude.extend(arguments);

      // The agent lives as long as the session: `exec` blocks until Claude
      // exits, and the flag stops it as soon as it does.
      let stop = AtomicBool::new(false);
      let spool = Spool::new(session.host_spool());

      let code = std::thread::scope(|scope| {
        if session.manifest.host.has_commands() {
          scope.spawn(|| {
            if let Err(error) = host::serve(
              &spool,
              &session.manifest.host.commands,
              &project_dir,
              session.manifest.host.concurrency,
              &stop,
            ) {
              eprintln!("compostbin: the host command agent stopped: {error}");
            }
          });
        }

        if !forwards.is_empty() {
          // `Listening` was said before Claude took the terminal; anything
          // printed now draws over it, so only what needs attention is.
          scope.spawn(|| {
            host::relay(&forwards, &stop, &|event| {
              if !matches!(event, PortEvent::Listening(_)) {
                report_port(event);
              }
            })
          });
        }

        let code = engine.exec(&session.exec_spec(&claude));
        stop.store(true, Ordering::Relaxed);
        code
      })?;

      // Best effort: a killed session leaves the spool behind, which is what
      // `compostbin clean` is for.
      if let Err(error) = session.clean(false) {
        eprintln!("compostbin: could not clean up after the session: {error}");
      }

      Ok(code)
    }

    Command::Shell => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      Ok(CliEngine::new().exec(&session.exec_spec(&["bash".to_string()]))?)
    }

    Command::Stop => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      let engine = CliEngine::new();
      let name = session.container_name();
      engine.stop(&name)?;
      engine.delete(&name)?;
      session.clean(false)?;
      println!("stopped {name}");
      Ok(0)
    }
  }
}

/// Builds the base image, naming where the Dockerfile landed so a failed build
/// can be retried by hand.
fn build_base_image(session: &Session) -> Result<i32, Box<dyn Error>> {
  let context = image::context(session);
  println!("building {} from {}", session.manifest.project.image, context.display());

  if !session.manifest.image.is_empty() {
    println!(
      "then {} from {}, for this project's own additions",
      session.image(),
      image::project_context(session).display()
    );
  }

  Ok(image::build(session, &CliEngine::new())?)
}

/// Prints every check, exiting non-zero when any of them failed so `doctor` is
/// usable as a precondition in a script.
fn report_diagnosis(session: &Session) -> Result<i32, Box<dyn Error>> {
  let checks = doctor::diagnose(
    session,
    &CliEngine::new(),
    &Keychain,
    std::env::var_os("ANTHROPIC_API_KEY").is_some(),
  );

  print!("{}", report::Diagnosis(&checks));

  Ok(i32::from(checks.iter().any(|check| check.status == Status::Fail)))
}

fn load_session(
  manifest_path: &std::path::Path,
  resolver: PathResolver,
  project_dir: &PathBuf,
) -> Result<Session, Box<dyn Error>> {
  Ok(Session::new(Manifest::load(manifest_path)?, resolver, project_dir))
}
