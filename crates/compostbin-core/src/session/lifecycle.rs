//! Creating the container, attaching to it, and cleaning up after it.

use crate::error::{At, SessionError};
use crate::host::{self, PortEvent};
use crate::manifest::TomlFile;
use crate::session::credentials::{self, CredentialSource, SeedOutcome};
use crate::session::record::Record;
use crate::session::{Notice, Session, briefing, image, settings};
use compostbin_engine::engine::Engine;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// What `run` attaches.
const CLAUDE: &str = "claude";

/// The process attached to the session, the guest user it runs as, and whether
/// it gets a terminal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Process {
  /// The command and its arguments.
  pub argv: Vec<String>,
  /// Whether the guest process gets a terminal, which it can only have if the
  /// caller has one to give.
  pub tty: bool,
  pub user: String,
}

impl Process {
  /// What `run` attaches: Claude, on a terminal whenever the caller has one.
  pub fn claude(arguments: &[String]) -> Self {
    Self {
      argv: std::iter::once(CLAUDE.to_string())
        .chain(arguments.iter().cloned())
        .collect(),
      tty: true,
      user: image::USER.to_string(),
    }
  }

  /// What `exec` attaches: a command of the caller's own, reading and writing
  /// the caller's streams unless it asked for a terminal.
  pub fn command(argv: &[String], tty: bool, user: &str) -> Self {
    Self {
      argv: argv.to_vec(),
      tty,
      user: user.to_string(),
    }
  }

  /// Only Claude needs setup to succeed; anything else may be debugging it.
  fn needs_setup(&self) -> bool {
    self.argv.first().is_some_and(|command| command == CLAUDE)
  }
}

/// Best effort: a log that cannot be written must not take a port down with it.
fn append_to_log(log: &Path, event: &PortEvent) {
  use std::io::Write;

  if let Ok(mut file) = std::fs::File::options().append(true).create(true).open(log) {
    let _ = writeln!(file, "{event}");
  }
}

impl Session {
  /// Creates the container and records what it was created with. One step,
  /// because `doctor` can say nothing about unrecorded mounts.
  fn create(&self, engine: &impl Engine, notify: &(dyn Fn(Notice) + Sync)) -> Result<(), SessionError> {
    let spec = self.run_spec();

    // Said first: the unpack is the slow part of a first run, and a silent wait
    // looks like a hang.
    if !engine.is_unpacked(&spec.image)? {
      notify(Notice::Unpacking(spec.image.clone()));
    }

    // Every create mounts a briefing rendered from the manifest as it reads now.
    briefing::write(&self.managed_settings(), &self.manifest, &self.config_path())?;

    // Every guest client that could still be waiting on a response died with the
    // container this one replaces, so their leftovers are now provably nobody's.
    if let Some(spool) = self.spool() {
      spool.sweep()?;
    }

    engine.run(&spec)?;
    Record::of(&spec.mounts, &spec.sockets).save(&self.mount_record())?;

    Ok(())
  }

  /// Whether the container is up, and so whether `run` will attach rather than
  /// create. Anything whose work belongs to creation (binding the port sockets,
  /// above all) must ask first.
  pub fn is_running(&self, engine: &impl Engine) -> Result<bool, SessionError> {
    Ok(engine.is_running(&self.container_name())?)
  }

  /// Attaches `process` to the session, creating the container first unless it
  /// is already running, and returns the process's exit code.
  ///
  /// A second `run` joins the session rather than replacing it, and does
  /// nothing else: the host command agent, the port relay, and the cleanup on
  /// exit belong to the process that created the container. A joiner binding
  /// the sockets again would take them from the relay the container is using,
  /// and one cleaning up as it left would empty a spool still being served.
  ///
  /// The creator owns the container, Claude or not: when it exits, so does
  /// everything that joined.
  ///
  /// Attaching leaves the record alone. It describes the running container, not
  /// the manifest as it reads now, and `doctor` checks the gap between them.
  pub fn run(
    &self,
    engine: &impl Engine,
    credentials: &impl CredentialSource,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    self.prepare(credentials, notify)?;

    if self.is_running(engine)? {
      return Ok(engine.exec(&self.process_spec(process))?);
    }

    self.launch(engine, process, notify)
  }

  /// Seeds Claude's token and shares the host's settings into Claude's home: a
  /// mount source, so reachable whether or not the container is up.
  ///
  /// Created first, because the mount needs it whether or not there was
  /// anything to put in it: a host with no `~/.claude` and no Keychain entry
  /// would otherwise leave the container with a mount source that does not
  /// exist, which it cannot create for itself.
  fn prepare(&self, credentials: &impl CredentialSource, notify: &(dyn Fn(Notice) + Sync)) -> Result<(), SessionError> {
    let home = self.claude_home();
    std::fs::create_dir_all(&home).at(&home)?;

    let seeded = credentials::seed(
      &self.claude_home(),
      self.manifest.claude.seed_from_keychain,
      credentials,
    )?;

    if seeded == SeedOutcome::NotInKeychain {
      notify(Notice::NotInKeychain);
    }

    let shared = settings::share(
      &self.resolver.resolve(settings::HOST_CLAUDE_HOME),
      &self.claude_home(),
      &self.manifest.claude.shared,
    )?;

    if !shared.is_empty() {
      notify(Notice::Shared(shared));
    }

    Ok(())
  }

  /// Creates the container and serves it until `process` exits.
  ///
  /// The port sockets are bound first: each has to already be a socket when the
  /// container's relays are set up, and they are set up at creation. The agent
  /// and the relay then run beside the attach, and the flag stops them as soon
  /// as it returns.
  ///
  /// Threads rather than a process of their own: the container dies with this
  /// process, so anything holding its ports afterwards would be holding them for
  /// nobody.
  fn launch(
    &self,
    engine: &impl Engine,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    self.prepare_host_spool()?;

    let bound = host::bind_all(&self.forwards(), &|event| notify(Notice::Port(event))).at(self.port_sockets())?;

    self.create(engine, notify)?;

    let stop = AtomicBool::new(false);
    let spool = self.spool();
    let log = self.ports_log();
    let log_port = |event: PortEvent| append_to_log(&log, &event);

    let code = std::thread::scope(|scope| {
      if let Some(spool) = &spool {
        scope.spawn(|| {
          if let Err(error) = host::serve(
            spool,
            &self.manifest.host.served_commands(),
            &self.project_dir,
            self.manifest.host.concurrency,
            &stop,
          ) {
            notify(Notice::AgentStopped(error));
          }
        });
      }

      if !bound.is_empty() {
        scope.spawn(|| host::relay(&bound, &stop, &log_port));
      }

      let code = match self.set_up(engine, process, notify) {
        Ok(0) => engine
          .exec(&self.process_spec(process))
          .map_err(SessionError::from),
        failed => failed,
      };
      stop.store(true, Ordering::Relaxed);
      code
    })?;

    // Best effort: a killed session leaves the spool behind, which is what
    // `compostbin clean` is for.
    if let Err(error) = self.clean_after_exit() {
      notify(Notice::CleanupFailed(error));
    }

    Ok(code)
  }

  /// Runs `[container] setup`, stopping at the first failing line. Returns its
  /// exit code if `process` needs setup, else 0.
  ///
  /// Called once the host agent and relay are up, so a line may use them. Same
  /// shell as `[image] run`.
  fn set_up(
    &self,
    engine: &impl Engine,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    for line in &self.manifest.container.setup {
      let code = engine.exec(&self.setup_spec(line))?;

      if code != 0 {
        notify(Notice::SetupFailed {
          line: line.clone(),
          code,
        });
        return Ok(if process.needs_setup() { code } else { 0 });
      }
    }

    Ok(0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::fixtures;
  use crate::host::Spool;
  use crate::manifest::{MANIFEST_RELATIVE_PATH, PathEntry};
  use crate::session::credentials::FakeSource;
  use crate::session::fixtures::{MANIFEST, PROJECT};
  use compostbin_engine::fake::{Call, RecordingEngine};
  use std::os::unix::fs::FileTypeExt;

  fn quiet(_: Notice) {}

  fn run(session: &Session, engine: &RecordingEngine) -> i32 {
    session
      .run(engine, &FakeSource(None), &Process::claude(&[]), &quiet)
      .expect("run should succeed")
  }

  fn claude(session: &Session) -> Call {
    Call::Exec(session.process_spec(&Process::claude(&[])))
  }

  fn root_shell() -> Process {
    Process::command(&["bash".to_string()], true, "root")
  }

  #[test]
  fn run_attaches_to_an_already_running_container() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let engine = RecordingEngine::with_running(&["compostbin-cb"]);

    run(&session, &engine);

    assert_eq!(
      engine.calls(),
      [Call::IsRunning("compostbin-cb".to_string()), claude(&session)],
      "a running container must not be recreated"
    );
  }

  #[test]
  fn run_attaches_another_process_to_a_running_container() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let engine = RecordingEngine::with_running(&["compostbin-cb"]);

    session
      .run(&engine, &FakeSource(None), &root_shell(), &quiet)
      .expect("run should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::IsRunning("compostbin-cb".to_string()),
        Call::Exec(session.process_spec(&root_shell())),
      ]
    );
  }

  /// A shell can start the session as well as Claude can.
  #[test]
  fn run_creates_a_container_for_another_process() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let engine = RecordingEngine::new();

    session
      .run(&engine, &FakeSource(None), &root_shell(), &quiet)
      .expect("run should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::IsRunning("compostbin-cb".to_string()),
        Call::IsUnpacked(session.run_spec().image),
        Call::Run(session.run_spec()),
        Call::Exec(session.process_spec(&root_shell())),
      ]
    );
    assert_eq!(session.process_spec(&root_shell()).arguments, ["bash".to_string()]);
    assert_eq!(session.process_spec(&root_shell()).user, Some("root".to_string()));
  }

  /// Another session's container is not this one: `run` creates its own.
  #[test]
  fn run_creates_a_container_that_is_not_running() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let engine = RecordingEngine::with_running(&["compostbin-other"]);

    run(&session, &engine);

    assert_eq!(
      engine.calls(),
      [
        Call::IsRunning("compostbin-cb".to_string()),
        Call::IsUnpacked(session.run_spec().image),
        Call::Run(session.run_spec()),
        claude(&session),
      ]
    );
  }

  /// Only an image's first run unpacks it, and only that one says so.
  #[test]
  fn run_says_when_it_unpacks_the_image_first() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let image = session.run_spec().image;
    let unpacking = |engine: &RecordingEngine| {
      let said = std::sync::Mutex::new(Vec::new());

      session
        .run(engine, &FakeSource(None), &Process::claude(&[]), &|notice| {
          if let Notice::Unpacking(image) = notice {
            said.lock().expect("unpoisoned").push(image);
          }
        })
        .expect("run should succeed");

      said.into_inner().expect("unpoisoned")
    };

    assert_eq!(unpacking(&RecordingEngine::new()), [image.clone()]);
    assert_eq!(
      unpacking(&RecordingEngine::with_unpacked(&[&image])),
      Vec::<String>::new()
    );
  }

  #[test]
  fn run_passes_its_arguments_through_to_claude() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let engine = RecordingEngine::new();

    let process = Process::claude(&["--continue".to_string()]);

    session
      .run(&engine, &FakeSource(None), &process, &quiet)
      .expect("run should succeed");

    assert_eq!(engine.calls().last(), Some(&Call::Exec(session.process_spec(&process))));
    assert_eq!(
      session.process_spec(&process).arguments,
      ["claude".to_string(), "--continue".to_string()]
    );
    assert_eq!(session.process_spec(&process).user, Some("claude".to_string()));
  }

  #[test]
  fn run_sets_up_a_new_container_before_starting_claude() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.container.setup = vec!["./bin/setup".to_string(), "true".to_string()];
    let engine = RecordingEngine::new();

    run(&session, &engine);

    assert_eq!(
      engine.calls()[3..],
      [
        Call::Exec(session.setup_spec("./bin/setup")),
        Call::Exec(session.setup_spec("true")),
        claude(&session),
      ]
    );
  }

  /// The creator already set it up.
  #[test]
  fn joining_a_running_session_sets_nothing_up() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.container.setup = vec!["./bin/setup".to_string()];
    let engine = RecordingEngine::with_running(&["compostbin-cb"]);

    run(&session, &engine);

    assert_eq!(engine.calls().last(), Some(&claude(&session)));
    assert_eq!(engine.calls().len(), 2);
  }

  #[test]
  fn failed_setup_does_not_start_claude() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.container.setup = vec!["false".to_string(), "true".to_string()];
    let engine = RecordingEngine::exiting_with(3);
    let said = std::sync::Mutex::new(Vec::new());

    let code = session
      .run(&engine, &FakeSource(None), &Process::claude(&[]), &|notice| {
        if let Notice::SetupFailed { line, code } = notice {
          said.lock().expect("unpoisoned").push((line, code));
        }
      })
      .expect("run should succeed");

    assert_eq!(code, 3);
    assert_eq!(said.into_inner().expect("unpoisoned"), [("false".to_string(), 3)]);
    assert_eq!(engine.calls().last(), Some(&Call::Exec(session.setup_spec("false"))));
  }

  /// A shell may be there to find out why setup fails.
  #[test]
  fn failed_setup_still_starts_another_process() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.container.setup = vec!["false".to_string(), "true".to_string()];
    let engine = RecordingEngine::exiting_with(3);
    let said = std::sync::Mutex::new(Vec::new());

    session
      .run(&engine, &FakeSource(None), &root_shell(), &|notice| {
        if let Notice::SetupFailed { line, code } = notice {
          said.lock().expect("unpoisoned").push((line, code));
        }
      })
      .expect("run should succeed");

    assert_eq!(said.into_inner().expect("unpoisoned"), [("false".to_string(), 3)]);
    assert_eq!(
      engine.calls()[3..],
      [
        Call::Exec(session.setup_spec("false")),
        Call::Exec(session.process_spec(&root_shell())),
      ],
      "setup stops at the failing line"
    );
  }

  /// Declared ports are bound before the container is created, since each has
  /// to already be a socket when its relay is set up.
  #[test]
  fn run_binds_the_port_sockets_before_creating_the_container() {
    // Under `/tmp` because macOS's own temp directory is deep enough to push a
    // socket in the session directory past the length a socket path may have.
    let temp = tempfile::Builder::new()
      .tempdir_in("/tmp")
      .expect("temp dir");
    let (_temp, mut session) = fixtures::session_in(temp, MANIFEST, PROJECT);
    session.manifest.host.ports = vec![7001];
    let engine = RecordingEngine::new();

    run(&session, &engine);

    assert!(
      std::fs::symlink_metadata(&session.forwards()[0].listen)
        .expect("the socket should have been bound")
        .file_type()
        .is_socket()
    );
  }

  /// Binding again would take the sockets from the relay the running container
  /// is using.
  #[test]
  fn joining_a_running_session_binds_nothing() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.host.ports = vec![7001];

    run(&session, &RecordingEngine::with_running(&["compostbin-cb"]));

    assert!(!session.port_sockets().exists());
  }

  #[test]
  fn run_says_when_there_is_no_token_to_seed() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    let said = std::sync::Mutex::new(Vec::new());
    let engine = RecordingEngine::with_unpacked(&[&session.run_spec().image]);

    session
      .run(&engine, &FakeSource(None), &Process::claude(&[]), &|notice| {
        said.lock().expect("lock").push(format!("{notice:?}"))
      })
      .expect("run should succeed");

    assert_eq!(said.into_inner().expect("lock"), ["NotInKeychain"]);
  }

  /// The mount source has to exist before the container does, and what it holds
  /// is rendered from the manifest this create ran with — an edited manifest
  /// reaches the session it creates, not the one after.
  #[test]
  fn creating_the_container_writes_the_briefing() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);

    run(&session, &RecordingEngine::new());

    assert_eq!(
      std::fs::read_to_string(session.managed_settings().join(briefing::BRIEFING_FILE))
        .expect("the briefing should exist"),
      briefing::briefing(&session.manifest, MANIFEST_RELATIVE_PATH)
    );
    assert!(
      session
        .managed_settings()
        .join(briefing::MANAGED_SETTINGS_FILE)
        .exists(),
      "nothing reads the briefing without the settings that name it"
    );
  }

  /// What `doctor`'s stale-mount check reads: the container's real mount set,
  /// which the manifest stops describing once edited.
  #[test]
  fn records_the_mounts_the_container_was_created_with() {
    let (temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    let base = temp.path().canonicalize().expect("canonical temp");
    let engine = RecordingEngine::new();

    run(&session, &engine);

    let recorded = Record::load_if_present(&session.mount_record())
      .expect("load should succeed")
      .expect("run must have written a record");
    assert_eq!(recorded, Record::of(&session.mounts(), &session.sockets()));

    // The container still has the mounts it was created with.
    session.manifest.paths.push(PathEntry {
      local: false,
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });

    assert!(
      !recorded
        .drift(&session.mounts(), &session.sockets())
        .is_empty(),
      "a path added mid-session is not mounted until the container is recreated"
    );
  }

  /// A killed client's leftovers go when its container is replaced, not while
  /// that container is still up.
  #[test]
  fn creating_a_container_sweeps_the_last_ones_leftovers() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    session.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");
    session
      .prepare_host_spool()
      .expect("prepare should succeed");

    let spool = Spool::new(session.host_spool());
    let orphan = spool.responses().join("0001.out.000001");
    std::fs::write(&orphan, "stranded\n").expect("write orphan");

    run(&session, &RecordingEngine::with_running(&["compostbin-cb"]));
    assert!(
      orphan.exists(),
      "a running container's responses are not a joiner's to delete, on the way in or out"
    );

    run(&session, &RecordingEngine::new());
    assert!(!orphan.exists(), "a replaced container's are");
  }

  /// The same edit, once the container has actually been recreated.
  #[test]
  fn recreating_the_container_records_the_new_mounts() {
    let (temp, mut session) = fixtures::session(MANIFEST, PROJECT);
    let base = temp.path().canonicalize().expect("canonical temp");

    run(&session, &RecordingEngine::new());
    session.manifest.paths.push(PathEntry {
      local: false,
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });
    // The first session has exited, taking its container with it.
    run(&session, &RecordingEngine::new());

    let recorded = Record::load_if_present(&session.mount_record())
      .expect("load should succeed")
      .expect("run must have rewritten the record");

    assert!(
      recorded
        .drift(&session.mounts(), &session.sockets())
        .is_empty(),
      "the record must describe the container that is running now: {recorded:?}"
    );
  }
}
