use crate::error::{At, SessionError};
use crate::host::{self, PortEvent};
use crate::image;
use crate::manifest::{TomlFile, USER_CONFIG_PATH, UserConfig};
use crate::session::credentials::{self, CredentialSource};
use crate::session::record::Record;
use crate::session::{Notice, Session, briefing, settings};
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

const CLAUDE: &str = "claude";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Process {
  pub argv: Vec<String>,
  pub tty: bool,
  pub user: String,
}

impl Process {
  pub fn claude(arguments: Vec<String>) -> Self {
    Self {
      argv: std::iter::once(CLAUDE.to_string())
        .chain(arguments)
        .collect(),
      tty: true,
      user: image::USER.to_string(),
    }
  }

  pub fn command(argv: Vec<String>, tty: bool, user: String) -> Self {
    Self { argv, tty, user }
  }

  /// Only Claude requires setup to succeed, since another process may be debugging it.
  fn needs_setup(&self) -> bool {
    self.argv.first().is_some_and(|command| command == CLAUDE)
  }
}

/// Ignores errors so a failed log write cannot stop the relay.
fn append_to_log(log: &Path, event: &PortEvent) {
  use std::io::Write;

  if let Ok(mut file) = std::fs::File::options().append(true).create(true).open(log) {
    let _ = writeln!(file, "{event}");
  }
}

impl Session {
  /// Always records the spec, since `doctor` relies on the record.
  fn create(&self, engine: &impl Engine, notify: &(dyn Fn(Notice) + Sync)) -> Result<(), SessionError> {
    let spec = self.run_spec();

    // Booting with a missing blob would fail naming the image's digest, not the blob's.
    if let Some(digest) = engine.missing_content(&spec.image)? {
      return Err(
        EngineError::unavailable(
          format!("boot from {}", spec.image),
          format!("blob {digest} is missing; run `compostbin build`"),
        )
        .into(),
      );
    }

    if !engine.is_unpacked(&spec.image)? {
      notify(Notice::Unpacking(spec.image.clone()));
    }

    briefing::write(&self.managed_settings(), &self.manifest, &self.config_path())?;

    // Safe only here: every client that could own a leftover died with the replaced container.
    if let Some(spool) = self.spool() {
      spool.sweep()?;
    }

    engine.run(&spec)?;
    Record::of(&spec.mounts, &self.manifest.host.ports)
      .save(&self.mount_record())
      .map_err(SessionError::Record)?;

    Ok(())
  }

  /// Work that belongs to creation, such as relaying ports, must check this first.
  pub fn is_running(&self, engine: &impl Engine) -> bool {
    engine.is_running(&self.container_name())
  }

  /// Joining a running container only execs: the agent, port relay, record, and exit cleanup belong to
  /// the creator, which holds the VM.
  pub fn run(
    &self,
    engine: &impl Engine,
    credentials: &impl CredentialSource,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    self.prepare(credentials, notify)?;

    if self.is_running(engine) {
      return Ok(engine.exec(&self.process_spec(process))?);
    }

    self.launch(engine, process, notify)
  }

  /// Claude's home is created even when empty, because the guest cannot create a missing mount source.
  fn prepare(&self, credentials: &impl CredentialSource, notify: &(dyn Fn(Notice) + Sync)) -> Result<(), SessionError> {
    let home = self.claude_home();
    std::fs::create_dir_all(&home).at(&home)?;

    if credentials::seed(&home, self.manifest.claude.seed_from_keychain, credentials)? {
      notify(Notice::NotInKeychain);
    }

    let user_config = self.resolver.resolve(USER_CONFIG_PATH);
    let user = UserConfig::load_if_present(&user_config)
      .map_err(SessionError::Config)?
      .unwrap_or_default();
    let extra: Vec<String> = user
      .claude
      .shared
      .into_iter()
      .chain(self.manifest.claude.shared.iter().cloned())
      .collect();

    let shared = settings::share(&self.resolver.claude_home(), &home, &extra)?;

    if !shared.is_empty() {
      notify(Notice::Shared(shared));
    }

    Ok(())
  }

  /// The agent and relay are threads because the VM the guest connects to dies with this process.
  fn launch(
    &self,
    engine: &impl Engine,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    self.prepare_host_spool()?;
    self.create(engine, notify)?;

    let bound = host::listen_all(engine, &self.container_name(), &self.forwards(), &|event| {
      notify(Notice::Port(event))
    })?;

    let stop = AtomicBool::new(false);
    let spool = self.spool();
    let log = self.ports_log();
    let log_port = |event: PortEvent| append_to_log(&log, &event);

    let code = std::thread::scope(|scope| {
      if let Some(spool) = &spool {
        scope.spawn(|| {
          if let Err(error) = host::serve(
            spool,
            &host::served_commands(&self.manifest.host),
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

    if let Err(error) = self.clean_after_exit() {
      notify(Notice::CleanupFailed(error));
    }

    Ok(code)
  }

  /// Returns the first failing line's code if `process` needs setup, else 0.
  fn set_up(
    &self,
    engine: &impl Engine,
    process: &Process,
    notify: &(dyn Fn(Notice) + Sync),
  ) -> Result<i32, SessionError> {
    for line in &self.manifest.container().setup {
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
  use crate::host::Spool;
  use crate::manifest::{MANIFEST_RELATIVE_PATH, PathEntry};
  use crate::session::credentials::FakeSource;
  use crate::session::fixtures::{self, MANIFEST, PROJECT};
  use compostbin_engine::fake::{Call, RecordingEngine};

  fn quiet(_: Notice) {}

  fn run(session: &Session, engine: &RecordingEngine) -> i32 {
    session
      .run(engine, &FakeSource(None), &Process::claude(Vec::new()), &quiet)
      .expect("run should succeed")
  }

  fn claude(session: &Session) -> Call {
    Call::Exec(session.process_spec(&Process::claude(Vec::new())))
  }

  fn root_shell() -> Process {
    Process::command(vec!["bash".to_string()], true, "root".to_string())
  }

  #[test]
  fn run_attaches_to_an_already_running_container() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
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
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
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

  #[test]
  fn run_creates_a_container_for_another_process() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let engine = RecordingEngine::new();

    session
      .run(&engine, &FakeSource(None), &root_shell(), &quiet)
      .expect("run should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::IsRunning("compostbin-cb".to_string()),
        Call::MissingContent(session.run_spec().image),
        Call::IsUnpacked(session.run_spec().image),
        Call::Run(session.run_spec()),
        Call::Exec(session.process_spec(&root_shell())),
      ]
    );
    assert_eq!(session.process_spec(&root_shell()).arguments, ["bash".to_string()]);
    assert_eq!(session.process_spec(&root_shell()).user, Some("root".to_string()));
  }

  #[test]
  fn run_creates_a_container_that_is_not_running() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let engine = RecordingEngine::with_running(&["compostbin-other"]);

    run(&session, &engine);

    assert_eq!(
      engine.calls(),
      [
        Call::IsRunning("compostbin-cb".to_string()),
        Call::MissingContent(session.run_spec().image),
        Call::IsUnpacked(session.run_spec().image),
        Call::Run(session.run_spec()),
        claude(&session),
      ]
    );
  }

  #[test]
  fn run_refuses_an_image_missing_a_blob_naming_it() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let engine = RecordingEngine::missing_content(&[], "sha256:abc");

    let error = session
      .run(&engine, &FakeSource(None), &Process::claude(Vec::new()), &quiet)
      .expect_err("a damaged image should not boot");

    assert!(
      error
        .to_string()
        .contains("blob sha256:abc is missing; run `compostbin build`"),
      "{error}"
    );
    assert!(
      !engine
        .calls()
        .iter()
        .any(|call| matches!(call, Call::Run(_))),
      "nothing was booted"
    );
  }

  #[test]
  fn run_says_when_it_unpacks_the_image_first() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let image = session.run_spec().image;
    let unpacking = |engine: &RecordingEngine| {
      let said = std::sync::Mutex::new(Vec::new());

      session
        .run(engine, &FakeSource(None), &Process::claude(Vec::new()), &|notice| {
          if let Notice::Unpacking(image) = notice {
            said.lock().expect("unpoisoned").push(image);
          }
        })
        .expect("run should succeed");

      said.into_inner().expect("unpoisoned")
    };

    assert_eq!(unpacking(&RecordingEngine::new()), std::slice::from_ref(&image));
    assert_eq!(
      unpacking(&RecordingEngine::with_unpacked(&[&image])),
      Vec::<String>::new()
    );
  }

  #[test]
  fn run_passes_its_arguments_through_to_claude() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let engine = RecordingEngine::new();

    let process = Process::claude(vec!["--continue".to_string()]);

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
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.shared_container_mut().setup = vec!["./bin/setup".to_string(), "true".to_string()];
    let engine = RecordingEngine::new();

    run(&session, &engine);

    assert_eq!(
      engine.calls()[4..],
      [
        Call::Exec(session.setup_spec("./bin/setup")),
        Call::Exec(session.setup_spec("true")),
        claude(&session),
      ]
    );
  }

  #[test]
  fn joining_a_running_session_sets_nothing_up() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.shared_container_mut().setup = vec!["./bin/setup".to_string()];
    let engine = RecordingEngine::with_running(&["compostbin-cb"]);

    run(&session, &engine);

    assert_eq!(engine.calls().last(), Some(&claude(&session)));
    assert_eq!(engine.calls().len(), 2);
  }

  #[test]
  fn failed_setup_does_not_start_claude() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.shared_container_mut().setup = vec!["false".to_string(), "true".to_string()];
    let engine = RecordingEngine::exiting_with(3);
    let said = std::sync::Mutex::new(Vec::new());

    let code = session
      .run(&engine, &FakeSource(None), &Process::claude(Vec::new()), &|notice| {
        if let Notice::SetupFailed { line, code } = notice {
          said.lock().expect("unpoisoned").push((line, code));
        }
      })
      .expect("run should succeed");

    assert_eq!(code, 3);
    assert_eq!(said.into_inner().expect("unpoisoned"), [("false".to_string(), 3)]);
    assert_eq!(engine.calls().last(), Some(&Call::Exec(session.setup_spec("false"))));
  }

  #[test]
  fn failed_setup_still_starts_another_process() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.shared_container_mut().setup = vec!["false".to_string(), "true".to_string()];
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
      engine.calls()[4..],
      [
        Call::Exec(session.setup_spec("false")),
        Call::Exec(session.process_spec(&root_shell())),
      ],
      "setup stops at the failing line"
    );
  }

  #[test]
  fn run_listens_for_the_ports_before_attaching() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.host.ports = vec![7001];
    let engine = RecordingEngine::new();

    run(&session, &engine);

    let calls = engine.calls();
    let position = |wanted: &Call| calls.iter().position(|call| call == wanted);
    let listened = position(&Call::Listen(
      session.container_name(),
      session.forwards()[0].vsock_port(),
    ))
    .expect("the port should have been listened for");

    assert!(position(&Call::Run(session.run_spec())) < Some(listened));
    assert!(Some(listened) < position(&claude(&session)));
  }

  #[test]
  fn joining_a_running_session_listens_for_nothing() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    session.manifest.host.ports = vec![7001];
    let engine = RecordingEngine::with_running(&["compostbin-cb"]);

    run(&session, &engine);

    assert!(
      !engine
        .calls()
        .iter()
        .any(|call| matches!(call, Call::Listen(..)))
    );
  }

  #[test]
  fn run_says_when_there_is_no_token_to_seed() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    let said = std::sync::Mutex::new(Vec::new());
    let engine = RecordingEngine::with_unpacked(&[&session.run_spec().image]);

    session
      .run(&engine, &FakeSource(None), &Process::claude(Vec::new()), &|notice| {
        said.lock().expect("lock").push(format!("{notice:?}"))
      })
      .expect("run should succeed");

    assert_eq!(said.into_inner().expect("lock"), ["NotInKeychain"]);
  }

  #[test]
  fn creating_the_container_writes_the_briefing() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);

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

  #[test]
  fn records_the_mounts_the_container_was_created_with() {
    let (temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    let base = temp.path().canonicalize().expect("canonical temp");
    let engine = RecordingEngine::new();

    run(&session, &engine);

    let recorded = Record::load_if_present(&session.mount_record())
      .expect("load should succeed")
      .expect("run must have written a record");
    assert_eq!(recorded, Record::of(&session.mounts(), &session.manifest.host.ports));

    session.manifest.paths.push(PathEntry {
      local: false,
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });

    assert!(
      !recorded
        .drift(&session.mounts(), &session.manifest.host.ports)
        .is_empty(),
      "a path added mid-session is not mounted until the container is recreated"
    );
  }

  #[test]
  fn creating_a_container_sweeps_the_last_ones_leftovers() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
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

  #[test]
  fn recreating_the_container_records_the_new_mounts() {
    let (temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);
    let base = temp.path().canonicalize().expect("canonical temp");

    run(&session, &RecordingEngine::new());
    session.manifest.paths.push(PathEntry {
      local: false,
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });
    run(&session, &RecordingEngine::new());

    let recorded = Record::load_if_present(&session.mount_record())
      .expect("load should succeed")
      .expect("run must have rewritten the record");

    assert!(
      recorded
        .drift(&session.mounts(), &session.manifest.host.ports)
        .is_empty(),
      "the record must describe the container that is running now: {recorded:?}"
    );
  }
}
