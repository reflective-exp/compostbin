pub mod cli;
mod report;

use apple_container::engine::{CliEngine, Engine};
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
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long `run` waits for the relay it started to have its sockets up. They
/// must be sockets before the container is created — `container` relays a
/// source that is already one, and mounts anything else as a plain file.
const RELAY_READY_TIMEOUT: Duration = Duration::from_secs(5);
const RELAY_READY_POLL: Duration = Duration::from_millis(25);

/// Starts the relay that holds this session's port sockets, unless one is
/// already holding them.
///
/// It is a separate process, detached from this terminal, because its sockets
/// belong to the container rather than to whoever is attached: the relay
/// `container` sets up is bound to the socket that existed when the container
/// was created, so a socket dropped and rebound mid-life is not reattached.
/// Leaving a session and starting another one must not cost the session its
/// ports, so the thing holding them outlives the attach and ends with the
/// container.
fn start_port_relay(session: &Session, project_dir: &std::path::Path) -> Result<(), Box<dyn Error>> {
  let forwards = session.forwards();

  if forwards.is_empty() || forwards.iter().all(host::served) {
    return Ok(());
  }

  let log = std::fs::File::options()
    .append(true)
    .create(true)
    .open(session.ports_log())?;

  let mut relay = std::process::Command::new(std::env::current_exe()?);
  relay
    .arg("port-relay")
    .current_dir(project_dir)
    .stdin(std::process::Stdio::null())
    .stdout(log.try_clone()?)
    .stderr(log)
    // Its own process group, so the Ctrl-C that interrupts Claude does not
    // reach the relay: the container is still there, and so are its ports.
    .process_group(0);
  relay.spawn()?;

  let deadline = Instant::now() + RELAY_READY_TIMEOUT;
  while !forwards.iter().all(host::served) {
    if Instant::now() > deadline {
      return Err(
        format!(
          "the port relay did not come up within {RELAY_READY_TIMEOUT:?}; see {}",
          session.ports_log().display()
        )
        .into(),
      );
    }
    std::thread::sleep(RELAY_READY_POLL);
  }

  Ok(())
}

/// What a session attaching to a container someone else started can say about
/// its ports. Normally the relay is right there, holding them; if it is not,
/// nothing this process does would reach the guest — the container's end is
/// bound to the socket that existed when it was created — so the fix is to
/// recreate the container, and saying so is all that is left.
fn report_adopted_ports(session: &Session) {
  let forwards = session.forwards();
  if forwards.is_empty() {
    return;
  }

  if forwards.iter().all(host::served) {
    println!("forwarding host ports {}", port_list(session));
    return;
  }

  eprintln!(
    "compostbin: host ports {} are declared but their relay is gone; `compostbin stop` then `compostbin run` restores them",
    port_list(session)
  );
}

fn port_list(session: &Session) -> String {
  session
    .manifest
    .host
    .ports
    .iter()
    .map(u16::to_string)
    .collect::<Vec<String>>()
    .join(", ")
}

/// What the relay says, in the terms a user would look for.
fn report_port(event: PortEvent) {
  match event {
    PortEvent::Listening(forward) => eprintln!(
      "compostbin: forwarding localhost:{} to {}",
      forward.port(),
      forward.upstream
    ),
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

      // The ports belong to whatever created the container: their sockets were
      // bound before it started, and nothing bound afterwards reaches it. So
      // this says how they stand and serves the spool.
      report_adopted_ports(&session);

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

      // Before the container is created, since the sockets have to be sockets
      // by the time `container run` reads them — and only when this run is the
      // one creating it. Attaching to a container that already has a relay
      // leaves that relay exactly where it is.
      if !session.is_running(&engine)? {
        start_port_relay(&session, &project_dir)?;
      }

      session.start(&engine)?;
      report_adopted_ports(&session);

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
              &session.manifest.host.served_commands(),
              &project_dir,
              session.manifest.host.concurrency,
              &stop,
            ) {
              eprintln!("compostbin: the host command agent stopped: {error}");
            }
          });
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

    Command::PortRelay => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      let forwards = session.forwards();

      if forwards.is_empty() {
        eprintln!("no [host] ports in {}; nothing to relay", manifest_path.display());
        return Ok(1);
      }

      // Another relay already holds them, which is the whole point of this
      // being a process of its own: it survived whatever started it.
      if forwards.iter().all(host::served) {
        eprintln!("the ports are already held by another relay");
        return Ok(0);
      }

      // Before binding, so a signal arriving immediately still unlinks the
      // sockets rather than leaving them for the next session to adopt.
      let stop = signals::stop_on_termination()?;
      let bound = host::bind_all(&forwards, &report_port)?;
      std::fs::write(session.ports_pid(), std::process::id().to_string())?;

      let engine = CliEngine::new();
      std::thread::scope(|scope| {
        scope.spawn(|| {
          host::watch(
            &session.container_name(),
            &engine,
            stop,
            host::APPEAR_GRACE,
            host::VANISH_GRACE,
          );
        });

        host::relay(&bound, stop, &report_port);
      });

      // The sockets are this process's: nothing else can tell a live one from
      // one left by a relay that died, so leaving them would make the next
      // session read a dead relay as a working one.
      drop(bound);
      let _ = std::fs::remove_dir_all(session.port_sockets());
      let _ = std::fs::remove_file(session.ports_pid());

      eprintln!("the container is gone; the relay is stopping");

      Ok(0)
    }

    Command::Shell => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      Ok(CliEngine::new().exec(&session.exec_spec(&["bash".to_string()]))?)
    }

    Command::Stop => {
      let session = load_session(&manifest_path, resolver, &project_dir)?;
      let engine = CliEngine::new();
      let name = session.container_name();
      session.remove_container(&engine)?;
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
