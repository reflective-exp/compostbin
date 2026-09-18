pub mod cli;
mod report;

use apple_container::builder::CliBuilder;
use apple_container::engine::Engine;
use clap::Parser;
use cli::{Arguments, Command};
use compostbin_core::doctor::{self, Status};
use compostbin_core::host::{self, PortEvent, Spool};
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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the relay says, in the terms a user would look for.
fn describe_port(event: &PortEvent) -> String {
  match event {
    PortEvent::Listening(forward) => format!("forwarding localhost:{} to {}", forward.port(), forward.upstream),
    PortEvent::UpstreamRefused(forward, error) => {
      format!("nothing answers at {}: {error}", forward.upstream)
    }
  }
}

/// For the events that happen before Claude is attached, when the terminal is
/// still ours to write to.
fn report_port(event: PortEvent) {
  eprintln!("compostbin: {}", describe_port(&event));
}

/// For the events that happen after.
///
/// A host service that has not started yet is an ordinary state — starting one
/// through `compostbin-host` is a reason a port would refuse for a while — so
/// the first connection to find it down must not draw over the session to say
/// so. It still has to be somewhere, because a port that never comes up looks
/// exactly the same from the guest.
fn log_port(log: &Path) -> impl Fn(PortEvent) + Sync + '_ {
  move |event| {
    use std::io::Write;

    if let Ok(mut file) = std::fs::File::options().append(true).create(true).open(log) {
      let _ = writeln!(file, "{}", describe_port(&event));
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
      local,
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

      match session.add(&canonical, readonly, local) {
        AddOutcome::AlreadyMounted { root } => {
          println!("{} is already mounted under {}", canonical.display(), root.display());
          Ok(0)
        }
        AddOutcome::NeedsRestart if restart => {
          save_manifest(&session, &manifest_path, local)?;
          Ok(session.restart(&select(&session)?)?)
        }
        AddOutcome::NeedsRestart => {
          save_manifest(&session, &manifest_path, local)?;
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
      let cleaned = session.clean(all)?;
      let verb = if all { "removed" } else { "cleared" };

      for path in cleaned {
        println!("{verb} {}", path.display());
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

      // Only the spool. A session's ports belong to the `run` that created the
      // VM and holds them for as long as it lives, so there is nothing here for
      // another process to take over.
      if !session.manifest.host.has_commands() {
        return Ok(1);
      }

      // Before the loop, so a signal arriving immediately stops the agent rather
      // than killing a claimed request.
      let stop = signals::stop_on_termination()?;

      println!(
        "serving {} host commands from {}",
        session.manifest.host.served_commands().len(),
        session.host_spool().display()
      );
      println!("SIGINT or SIGTERM stops it, a second one kills it");

      host::serve(
        &Spool::new(session.host_spool()),
        &session.manifest.host.served_commands(),
        &project_dir,
        session.manifest.host.concurrency,
        stop,
      )?;

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

      let engine = select(&session)?;
      session.prepare_host_spool()?;

      // Before the container is created: each source has to already be a socket
      // when the VM's relays are set up, and they are set up at start.
      let bound = host::bind_all(&session.forwards(), &report_port)?;

      session.start(&engine)?;

      let mut claude = vec!["claude".to_string()];
      claude.extend(arguments);

      // Both the agent and the relay live as long as the session: `exec` blocks
      // until Claude exits, and the flag stops them as soon as it does.
      //
      // Threads rather than a process of their own. Under the `container` CLI
      // the ports had to outlive this terminal, because the container did; the
      // VM now dies with this process, so anything holding its ports afterwards
      // would be holding them for nobody.
      let stop = AtomicBool::new(false);
      let spool = Spool::new(session.host_spool());

      let code = std::thread::scope(|scope| {
        if session.manifest.host.has_commands() {
          scope.spawn(|| {
            if let Err(error) = host::serve(
              &spool,
              &session.manifest.host.served_commands(),
              &project_dir,
              session.manifest.host.concurrency,
              &stop,
            ) {
              eprintln!("compostbin: the host command agent stopped: {error}");
            }
          });
        }

        if !bound.is_empty() {
          // The log path moves in; the flag and the sockets are shared with the
          // attach that ends them.
          let log = session.ports_log();
          let bound = &bound;
          let stop = &stop;

          scope.spawn(move || host::relay(bound, stop, &log_port(&log)));
        }

        let code = engine.exec(&session.exec_spec(&claude));
        stop.store(true, Ordering::Relaxed);
        code
      })?;

      // Best effort: a killed session leaves the spool behind, which is what
      // `compostbin clean` is for.
      if let Err(error) = session.clean_after_exit() {
        eprintln!("compostbin: could not clean up after the session: {error}");
      }

      Ok(code)
    }

    Command::Shell => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      Ok(select(&session)?.exec(&session.exec_spec(&["bash".to_string()]))?)
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

  // Not the engine: Containerization manages and pulls images but does not
  // build them, so BuildKit-in-a-container is still the CLI's job and always
  // will be.
  Ok(image::build(session, &CliBuilder::new())?)
}

/// Prints every check, exiting non-zero when any of them failed so `doctor` is
/// usable as a precondition in a script.
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

/// The engine a session runs on.
///
/// One implementation now: Containerization.framework, in this process. The
/// `container` CLI still builds images — nothing in the framework replaces
/// BuildKit — but it no longer runs anything.
#[cfg(target_os = "macos")]
fn select(session: &Session) -> Result<containerization_framework_bridge::FrameworkEngine, Box<dyn Error>> {
  let store = containerization_framework_bridge::Store::discover()?;

  // The control socket lives here, so the directory has to exist before `run`
  // binds it — earlier than anything else would have created it.
  std::fs::create_dir_all(session.state_dir())?;

  Ok(containerization_framework_bridge::FrameworkEngine::new(
    session.state_dir(),
    store,
  ))
}

/// compostbin only runs on macOS, but it has to compile inside its own Debian
/// guest — where `cargo check` is how a session checks its work. Nothing
/// constructs this; it exists so `select` has a type to fail with.
#[cfg(not(target_os = "macos"))]
mod unsupported {
  use apple_container::error::EngineError;
  use apple_container::model::{ExecSpec, RunSpec};

  pub struct Engine;

  impl apple_container::engine::Engine for Engine {
    fn containers(&self) -> Result<Vec<String>, EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn delete(&self, _name: &str) -> Result<(), EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn exec(&self, _spec: &ExecSpec) -> Result<i32, EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn images(&self) -> Result<Vec<String>, EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn run(&self, _spec: &RunSpec) -> Result<String, EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn running_containers(&self) -> Result<Vec<String>, EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn stop(&self, _name: &str) -> Result<(), EngineError> {
      unreachable!("no session runs off macOS")
    }

    fn version(&self) -> Result<Option<String>, EngineError> {
      unreachable!("no session runs off macOS")
    }
  }
}

#[cfg(not(target_os = "macos"))]
fn select(_session: &Session) -> Result<unsupported::Engine, Box<dyn Error>> {
  Err("Containerization.framework is macOS only".into())
}

/// Writes back whichever file the new entry belongs to, leaving the other
/// untouched.
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
