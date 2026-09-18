//! One project's container, and the state that outlives it.
//!
//! The submodules are what a session owns rather than uses: its image, its
//! token, the host Claude config it starts from, and the record of what its
//! container was created with. Mounts are built here, from the workspace.

pub mod briefing;
pub mod credentials;
pub mod image;
pub mod record;
pub mod settings;

use crate::error::{At, PathError, SessionError};
use crate::host::{Forward, GUEST_PORTS_TARGET, GUEST_SPOOL_TARGET, Spool};
use crate::manifest::{Manifest, PathEntry, SESSIONS_DIR};
use crate::session::briefing::{MANAGED_SETTINGS_DIR, MANAGED_SETTINGS_TARGET};
use crate::session::image::GUEST_PORTS_NAME;
use crate::session::record::{RECORD_FILE, Record};
use crate::workspace::paths::{PathResolver, root_containing};
use crate::workspace::{Origin, Workspace};
use compostbin_engine::engine::Engine;
use compostbin_engine::model::{EnvVar, ExecSpec, Mount, RunSpec, SocketRelay};
use std::path::{Path, PathBuf};

/// Where Claude's home is mounted inside the container, which runs as `claude`.
pub const CLAUDE_HOME_TARGET: &str = "/home/claude/.claude";
/// Under the session state directory: kept, because `claude --continue` reads it.
pub const CLAUDE_HOME_DIR: &str = "claude-home";
/// Under the session state directory: emptied by `clean`, holding only requests
/// in flight. Emptied and not removed — the container mounts it.
pub const SPOOL_DIR: &str = "host";
/// Under the session state directory: one bound socket per declared port, which
/// live exactly as long as the container created with them — and so outlive the
/// `run` that created it.
pub const PORTS_DIR: &str = "ports";
/// Where the relay says what it could not, while a session is attached.
pub const PORTS_LOG: &str = "ports.log";
/// Keeps a detached container alive so `exec` has something to attach to.
pub const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "infinity"];
pub const NAME_PREFIX: &str = "compostbin-";
/// Names no real compositor: nothing in the guest draws, it only has to be set.
pub const CLIPBOARD_DISPLAY: &str = "compostbin-clipboard";

/// Empties a directory without unlinking it, or removes a plain file.
///
/// Every transient path a session keeps is also a mount source, and a running
/// container's mount is attached to the inode it was created with: a directory
/// removed and recreated at the same path is never reattached, so the guest is
/// left holding one that nothing writes to any more. Emptying is the only kind
/// of cleaning a live mount survives.
///
/// A symlink is removed rather than followed, so a link planted where a mount
/// source belongs cannot make this clear something else.
fn clear(target: &Path) -> Result<(), PathError> {
  let kind = std::fs::symlink_metadata(target).at(target)?.file_type();

  if !kind.is_dir() {
    return std::fs::remove_file(target).at(target);
  }

  for entry in std::fs::read_dir(target).at(target)? {
    let entry = entry.at(target)?;
    let path = entry.path();

    let outcome = if entry.file_type().at(&path)?.is_dir() {
      std::fs::remove_dir_all(&path)
    } else {
      std::fs::remove_file(&path)
    };

    outcome.at(&path)?;
  }

  Ok(())
}

/// What `add` did, and therefore what the caller must do next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddOutcome {
  /// Already inside a mounted root — visible in the container right now.
  AlreadyMounted { root: PathBuf },
  /// Recorded in the manifest, but invisible until the container is recreated.
  NeedsRestart,
}

pub struct Session {
  pub manifest: Manifest,
  project_dir: PathBuf,
  resolver: PathResolver,
}

impl Session {
  pub fn new(manifest: Manifest, resolver: PathResolver, project_dir: impl Into<PathBuf>) -> Self {
    Self {
      manifest,
      project_dir: project_dir.into(),
      resolver,
    }
  }

  /// Records a path in the manifest unless a mounted root already covers it.
  /// `path` must be canonical: a symlink out of a root looks contained and is
  /// not. `local` records it in the uncommitted manifest instead of the one the
  /// project shares.
  pub fn add(&mut self, path: impl Into<PathBuf>, readonly: bool, local: bool) -> AddOutcome {
    let path = path.into();

    if let Some(root) = root_containing(&path, &self.resolved_roots()) {
      return AddOutcome::AlreadyMounted {
        root: root.to_path_buf(),
      };
    }

    self.manifest.paths.push(PathEntry {
      local,
      readonly,
      source: path.display().to_string(),
      target: None,
    });

    AddOutcome::NeedsRestart
  }

  /// Resolves a manifest path string against this session's cwd and home, so
  /// callers need no `PathResolver` of their own.
  pub fn resolve(&self, raw: &str) -> PathBuf {
    self.resolver.resolve(raw)
  }

  pub fn resolver(&self) -> &PathResolver {
    &self.resolver
  }

  /// The source side of the bind mount, and so where credentials are seeded
  /// before the container starts.
  ///
  /// Per session unless the manifest overrides it: one shared home would make
  /// `--continue` resume whichever project ran last, and expose one project's
  /// history to another's container.
  pub fn claude_home(&self) -> PathBuf {
    match &self.manifest.claude.home {
      Some(home) => self.resolver.resolve(home),
      None => self.state_dir().join(CLAUDE_HOME_DIR),
    }
  }

  /// Everything this session keeps on the host, under its own container name.
  pub fn state_dir(&self) -> PathBuf {
    self
      .resolver
      .resolve(SESSIONS_DIR)
      .join(self.container_name())
  }

  /// What `stop` and `clean` may delete: state that means nothing once the
  /// container is gone. Not Claude's home — that holds the conversation
  /// `--continue` reattaches to.
  fn transient_state(&self) -> Vec<PathBuf> {
    vec![
      self.host_spool(),
      self.port_sockets(),
      self.ports_log(),
      self.managed_settings(),
    ]
  }

  /// Empties this session's transient state, and with `everything` removes the
  /// session directory whole. Missing paths are not an error: `clean` exists
  /// because an earlier exit may not have run.
  ///
  /// Emptied rather than removed, because the container may still be running
  /// and every one of these paths is a mount source (§`clear`). `--all` is the
  /// exception: it discards the conversation too, so the session it belonged to
  /// is over by definition.
  pub fn clean(&self, everything: bool) -> Result<Vec<PathBuf>, PathError> {
    if everything {
      let state = self.state_dir();
      if !state.exists() {
        return Ok(Vec::new());
      }
      std::fs::remove_dir_all(&state).at(&state)?;
      return Ok(vec![state]);
    }

    let spool = self.host_spool();
    let mut cleared = Vec::new();

    for target in self.transient_state() {
      if !target.exists() {
        continue;
      }

      if target == spool {
        Spool::new(&target).empty()?;
      } else {
        clear(&target)?;
      }

      cleared.push(target);
    }

    Ok(cleared)
  }

  /// What `run` may delete when Claude exits. Narrower than `clean`: the
  /// container is still running, with the managed settings mounted, and the next
  /// `run` attaches to it without writing them again.
  ///
  /// The port sockets stay: they belong to the container, which is still
  /// running, and to the relay holding them — which outlives this process for
  /// exactly that reason. The spool is emptied rather than removed, for the
  /// same reason in a different shape: the container is holding that mount, and
  /// unlinking the directory would leave every later `shell` and `host-agent`
  /// for this container talking to an inode nothing can reach.
  pub fn clean_after_exit(&self) -> Result<Vec<PathBuf>, PathError> {
    let spool = self.host_spool();
    if !spool.exists() {
      return Ok(Vec::new());
    }

    Spool::new(&spool).empty()?;

    Ok(vec![spool])
  }

  pub fn container_name(&self) -> String {
    let project = self
      .manifest
      .project
      .name
      .clone()
      .or_else(|| {
        self
          .project_dir
          .file_name()
          .map(|basename| basename.to_string_lossy().into_owned())
      })
      .unwrap_or_default();

    format!("{NAME_PREFIX}{project}")
  }

  /// The host paths this session exposes, in declaration order: roots, the
  /// project directory when no root covers it, then explicit `[[paths]]`. Order
  /// matters — a name goes to the first entry that claims it.
  pub fn workspace(&self) -> Workspace {
    let mut workspace = Workspace::new();

    for root in &self.manifest.workspace.roots {
      workspace.push(self.resolver.resolve(root), None, Origin::Root, false);
    }

    if root_containing(&self.project_dir, &self.resolved_roots()).is_none() {
      workspace.push(self.project_dir.clone(), None, Origin::Project, false);
    }

    for entry in &self.manifest.paths {
      let target = entry.target.as_ref().map(PathBuf::from);
      let origin = if entry.local { Origin::Local } else { Origin::Explicit };
      workspace.push(self.resolver.resolve(&entry.source), target, origin, entry.readonly);
    }

    workspace
  }

  /// Every workspace entry, then the spool, then Claude's home. Order is
  /// preserved so a nested mount declared later lands on top of its parent.
  pub fn mounts(&self) -> Vec<Mount> {
    let mut mounts: Vec<Mount> = self
      .workspace()
      .entries()
      .iter()
      .map(|entry| Mount {
        readonly: entry.readonly,
        source: entry.host.clone(),
        target: entry.guest.clone(),
      })
      .collect();

    // With no allowlist there is no channel at all, rather than an empty one.
    if self.manifest.host.has_commands() {
      mounts.push(Mount {
        readonly: false,
        source: self.host_spool(),
        target: PathBuf::from(GUEST_SPOOL_TARGET),
      });
    }

    // Unconditional, unlike the spool: a session with no host commands still
    // needs to know it is in a container. Read-only, because the guest changing
    // what it is told is the whole point of managed settings.
    mounts.push(Mount {
      readonly: true,
      source: self.managed_settings(),
      target: PathBuf::from(MANAGED_SETTINGS_TARGET),
    });

    mounts.push(Mount {
      readonly: false,
      source: self.claude_home(),
      target: PathBuf::from(CLAUDE_HOME_TARGET),
    });

    mounts
  }

  /// Where the session lands inside the container. The fallback to `/workspace`
  /// keeps a misconfigured manifest from starting in an unmounted directory.
  /// One socket per declared port, carried into the guest rather than mounted
  /// there, and named after the port so the guest's relay finds it without
  /// being told where to look.
  ///
  /// Each has to be a live socket by the time the container is created, which
  /// is why `run` starts the relay that binds them first.
  pub fn sockets(&self) -> Vec<SocketRelay> {
    self
      .forwards()
      .into_iter()
      .map(|forward| SocketRelay {
        target: PathBuf::from(GUEST_PORTS_TARGET).join(
          forward
            .listen
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default(),
        ),
        source: forward.listen,
      })
      .collect()
  }

  pub fn workdir(&self) -> PathBuf {
    self
      .workspace()
      .guest_path(&self.project_dir)
      .unwrap_or_else(|| PathBuf::from(crate::workspace::WORKSPACE_TARGET))
  }

  /// Inside the session directory, so concurrent projects cannot see each
  /// other's requests and `clean` takes it with the rest.
  pub fn host_spool(&self) -> PathBuf {
    self.state_dir().join(SPOOL_DIR)
  }

  /// Where this session's port sockets are bound. Inside the session directory
  /// for the same reason as the spool: the directory is what confines them, the
  /// socket mode cannot be (the guest end is root-owned, and the session is
  /// not).
  pub fn port_sockets(&self) -> PathBuf {
    self.state_dir().join(PORTS_DIR)
  }

  /// Where the relay writes once Claude is attached.
  ///
  /// A file rather than the terminal, because by then the terminal is Claude's:
  /// the relay runs on a thread of `run`, and anything it printed would land in
  /// the middle of what Claude is drawing. A host service that has not started
  /// yet is an ordinary state — `[host.commands]` is one of the ways it starts
  /// — so the first connection to refuse must not look like a fault.
  pub fn ports_log(&self) -> PathBuf {
    self.state_dir().join(PORTS_LOG)
  }

  /// The source side of the managed settings mount: what this session tells its
  /// Claude about itself. Per session, because it is rendered from this
  /// project's manifest.
  pub fn managed_settings(&self) -> PathBuf {
    self.state_dir().join(MANAGED_SETTINGS_DIR)
  }

  /// A derived image when the manifest adds packages or build steps, otherwise
  /// the shared base itself.
  pub fn image(&self) -> String {
    if self.manifest.image.is_empty() {
      self.manifest.project.image.clone()
    } else {
      format!("compostbin/{}:latest", self.container_name())
    }
  }

  /// Each declared port, as a socket in this session's directory relayed to the
  /// host's loopback.
  pub fn forwards(&self) -> Vec<Forward> {
    self
      .manifest
      .host
      .ports
      .iter()
      .map(|&port| Forward::to_loopback(&self.port_sockets(), port))
      .collect()
  }

  /// The container's own process: the port relay when ports are declared —
  /// which fixes them at creation, like the mounts — and otherwise only
  /// something to keep it alive for `exec`.
  fn process(&self) -> Vec<String> {
    if !self.manifest.host.has_ports() {
      return KEEPALIVE_COMMAND
        .iter()
        .copied()
        .map(str::to_string)
        .collect();
    }

    std::iter::once(GUEST_PORTS_NAME.to_string())
      .chain(self.manifest.host.ports.iter().map(u16::to_string))
      .collect()
  }

  pub fn run_spec(&self) -> RunSpec {
    RunSpec {
      arguments: self.process(),
      cpus: Some(self.manifest.container.cpus),
      detach: true,
      env: self
        .manifest
        .container
        .env
        .iter()
        .map(|name| EnvVar::Inherit(name.clone()))
        .collect(),
      image: self.image(),
      memory: Some(self.manifest.container.memory.clone()),
      mounts: self.mounts(),
      name: self.container_name(),
      sockets: self.sockets(),
      workdir: Some(self.workdir()),
    }
  }

  /// The mount source must exist before the container starts, and the guest
  /// cannot create it. A no-op with no commands declared.
  pub fn prepare_host_spool(&self) -> Result<(), PathError> {
    if !self.manifest.host.has_commands() {
      return Ok(());
    }

    Spool::new(self.host_spool()).create()
  }

  /// Where the container's creation-time mounts are recorded, so `doctor` can
  /// tell a manifest edited mid-session from one that took effect.
  pub fn mount_record(&self) -> PathBuf {
    self.state_dir().join(RECORD_FILE)
  }

  /// Creates the container and records what it was created with. One step,
  /// because `doctor` can say nothing about unrecorded mounts.
  fn create(&self, engine: &impl Engine) -> Result<(), SessionError> {
    let spec = self.run_spec();

    // Here rather than in the CLI, so every path that creates a container —
    // `run`, `add --restart` — mounts a briefing rendered from the manifest as
    // it reads now.
    briefing::write(&self.managed_settings(), &self.manifest)?;

    // Every guest client that could still be waiting on a response died with the
    // container this one replaces, so their leftovers are now provably nobody's.
    if self.manifest.host.has_commands() {
      Spool::new(self.host_spool()).sweep()?;
    }

    engine.run(&spec)?;
    Record::of(&spec.mounts, &spec.sockets).save(&self.mount_record())?;

    Ok(())
  }

  /// Makes the container exist and be running. `container run` refuses a name
  /// that is taken, so a second `run` attaches rather than recreates; a stopped
  /// container is deleted first, its mounts no longer trustworthy.
  ///
  /// Attaching leaves the record alone: it describes the running container, not
  /// the manifest as it reads now, and `doctor` checks the gap between them.
  /// Whether the container is already up, and so whether `start` will attach to
  /// it rather than create it. Asked before starting by anything whose work
  /// belongs to the container's creation — binding the port sockets, above all.
  pub fn is_running(&self, engine: &impl Engine) -> Result<bool, SessionError> {
    Ok(
      engine
        .running_containers()?
        .contains(&self.container_name()),
    )
  }

  pub fn start(&self, engine: &impl Engine) -> Result<(), SessionError> {
    let name = self.container_name();

    if self.is_running(engine)? {
      return Ok(());
    }

    if engine.containers()?.contains(&name) {
      engine.delete(&name)?;
    }

    self.create(engine)
  }

  /// Stops the container if it is running and deletes it if it exists, so a
  /// container that already stopped, or was deleted by hand, is not an error.
  pub fn remove_container(&self, engine: &impl Engine) -> Result<(), SessionError> {
    let name = self.container_name();

    if engine.running_containers()?.contains(&name) {
      engine.stop(&name)?;
    }

    if engine.containers()?.contains(&name) {
      engine.delete(&name)?;
    }

    Ok(())
  }

  /// Recreates the container so a new mount takes effect, then reattaches to the
  /// same conversation — cheap, because Claude's home outlives the container.
  pub fn restart(&self, engine: &impl Engine) -> Result<i32, SessionError> {
    self.remove_container(engine)?;
    self.create(engine)?;

    Ok(engine.exec(&self.exec_spec(&["claude".to_string(), "--continue".to_string()]))?)
  }

  /// `IS_SANDBOX=1` tells Claude it is already sandboxed.
  ///
  /// `CLAUDE_CONFIG_DIR` moves `.claude.json` — the account, the onboarding
  /// answers, the per-project trust — into the bind-mounted home. It defaults to
  /// `~/.claude.json`, outside the mount, so without this every new container
  /// starts logged out however faithfully `~/.claude` is preserved.
  ///
  /// `WAYLAND_DISPLAY`, with `[host] clipboard`, is what makes Claude copy at
  /// all: on Linux it looks for `wl-copy` only when there is a display to copy
  /// to. There is none, but the guest's `wl-copy` sends to the host's.
  pub fn exec_spec(&self, arguments: &[String]) -> ExecSpec {
    let mut env = vec![
      EnvVar::Set {
        name: "CLAUDE_CONFIG_DIR".to_string(),
        value: CLAUDE_HOME_TARGET.to_string(),
      },
      EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      },
    ];

    if self.manifest.host.clipboard {
      env.push(EnvVar::Set {
        name: "WAYLAND_DISPLAY".to_string(),
        value: CLIPBOARD_DISPLAY.to_string(),
      });
    }

    ExecSpec {
      arguments: arguments.to_vec(),
      env,
      interactive: true,
      name: self.container_name(),
      tty: true,
      workdir: Some(self.workdir()),
    }
  }

  pub fn resolved_roots(&self) -> Vec<PathBuf> {
    self
      .manifest
      .workspace
      .roots
      .iter()
      .map(|root| self.resolver.resolve(root))
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use compostbin_engine::fake::{Call, RecordingEngine};
  use std::os::unix::fs::MetadataExt;
  use tempfile::TempDir;

  const MANIFEST: &str = r#"
[project]
name = "cb"

[container]
cpus   = 4
env    = ["ANTHROPIC_API_KEY"]
memory = "8G"

[workspace]
roots = ["~/workspace"]

[[paths]]
readonly = true
source   = "~/.cargo/registry"
"#;

  fn session() -> Session {
    Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new("/Users/user/workspace/compostbin", "/Users/user"),
      "/Users/user/workspace/compostbin",
    )
  }

  /// A session whose home really exists, for the tests that create a container
  /// and so write a mount record.
  fn session_under(base: &std::path::Path) -> Session {
    Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("workspace/compostbin"), base),
      base.join("workspace/compostbin"),
    )
  }

  #[test]
  fn add_inside_a_root_changes_nothing() {
    let mut session = session();

    let outcome = session.add("/Users/user/workspace/other", false, false);

    assert_eq!(
      outcome,
      AddOutcome::AlreadyMounted {
        root: "/Users/user/workspace".into()
      }
    );
    assert_eq!(session.manifest.paths.len(), 1, "no new [[paths]] entry");
    assert_eq!(session.mounts().len(), 4, "no new mount");
  }

  #[test]
  fn add_outside_every_root_records_a_path() {
    let mut session = session();

    let outcome = session.add("/Users/user/vendor/libfoo", true, false);

    assert_eq!(outcome, AddOutcome::NeedsRestart);
    assert_eq!(session.manifest.paths[1].source, "/Users/user/vendor/libfoo");
    assert_eq!(session.manifest.paths[1].readonly, true);
    assert_eq!(session.manifest.paths[1].target, None);
    assert!(
      session.mounts().contains(&Mount {
        readonly: true,
        source: "/Users/user/vendor/libfoo".into(),
        target: "/workspace/libfoo".into(),
      }),
      "the added path must become a mount"
    );
    assert_eq!(session.manifest.paths[1].local, false, "add records a shared path");
  }

  /// A local path mounts exactly like any other, and only `ls` tells them apart.
  #[test]
  fn a_local_path_mounts_and_says_where_it_came_from() {
    let mut session = session();

    session.add("/Users/user/vendor/libfoo", false, true);

    assert!(session.manifest.paths[1].local);
    let entry = session
      .workspace()
      .entries()
      .iter()
      .find(|entry| entry.host == PathBuf::from("/Users/user/vendor/libfoo"))
      .expect("the local path should be mounted")
      .clone();
    assert_eq!(entry.origin, Origin::Local);
    assert_eq!(entry.guest, PathBuf::from("/workspace/libfoo"));
  }

  #[test]
  fn start_attaches_to_an_already_running_container() {
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", true)]);

    session().start(&engine).expect("start should succeed");

    assert_eq!(
      engine.calls(),
      [Call::Running],
      "a running container must not be recreated"
    );
  }

  #[test]
  fn start_recreates_a_stopped_container() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = session_under(&base);
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", false)]);

    session.start(&engine).expect("start should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::Running,
        Call::Containers,
        Call::Delete("compostbin-cb".to_string()),
        Call::Run(session.run_spec()),
      ]
    );
  }

  #[test]
  fn start_creates_a_container_that_does_not_exist() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = session_under(&base);
    let engine = RecordingEngine::with_containers(&[("compostbin-other", true)]);

    session.start(&engine).expect("start should succeed");

    assert_eq!(
      engine.calls(),
      [Call::Running, Call::Containers, Call::Run(session.run_spec())],
      "there was nothing to delete"
    );
  }

  /// The mount source has to exist before the container does, and what it holds
  /// is rendered from the manifest this create ran with — an edited manifest
  /// reaches the session it creates, not the one after.
  #[test]
  fn creating_the_container_writes_the_briefing() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = session_under(&base);

    session
      .start(&RecordingEngine::new())
      .expect("start should succeed");

    assert_eq!(
      std::fs::read_to_string(session.managed_settings().join(briefing::BRIEFING_FILE))
        .expect("the briefing should exist"),
      briefing::briefing(&session.manifest)
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
  fn restarts_by_stopping_deleting_and_continuing() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = session_under(&base);
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", true)]);

    session.restart(&engine).expect("restart should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::Running,
        Call::Stop("compostbin-cb".to_string()),
        Call::Containers,
        Call::Delete("compostbin-cb".to_string()),
        Call::Run(session.run_spec()),
        Call::Exec(session.exec_spec(&["claude".to_string(), "--continue".to_string()])),
      ]
    );
  }

  #[test]
  fn removes_stopped_container() {
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", false)]);

    session()
      .remove_container(&engine)
      .expect("remove should succeed");

    assert_eq!(
      engine.calls(),
      [
        Call::Running,
        Call::Containers,
        Call::Delete("compostbin-cb".to_string())
      ]
    );
  }

  #[test]
  fn removes_missing_container() {
    let engine = RecordingEngine::with_containers(&[("compostbin-other", true)]);

    session()
      .remove_container(&engine)
      .expect("remove should succeed");

    assert_eq!(
      engine.calls(),
      [Call::Running, Call::Containers],
      "another session's container must be left alone"
    );
  }

  #[test]
  fn preserves_claude_home_across_restart() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = session_under(&base);
    let engine = RecordingEngine::new();

    session.restart(&engine).expect("restart should succeed");

    let Call::Run(spec) = &engine.calls()[2] else {
      panic!("the third call should have created the container: {:?}", engine.calls());
    };
    assert!(
      spec
        .mounts
        .iter()
        .any(|mount| { mount.source == session.claude_home() && mount.target == PathBuf::from(CLAUDE_HOME_TARGET) }),
      "the recreated container must remount Claude's home: {:?}",
      spec.mounts
    );
  }

  /// What `doctor`'s stale-mount check reads: the container's real mount set,
  /// which the manifest stops describing once edited.
  #[test]
  fn records_the_mounts_the_container_was_created_with() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let mut session = session_under(&base);
    let engine = RecordingEngine::new();

    session.start(&engine).expect("start should succeed");

    let recorded = Record::load(&session.mount_record())
      .expect("load should succeed")
      .expect("start must have written a record");
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
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let mut session = session_under(&base);
    session.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");
    session
      .prepare_host_spool()
      .expect("prepare should succeed");

    let spool = Spool::new(session.host_spool());
    let orphan = spool.responses().join("0001.out.000001");
    std::fs::write(&orphan, "stranded\n").expect("write orphan");

    session
      .start(&RecordingEngine::with_containers(&[("compostbin-cb", true)]))
      .expect("start should succeed");
    assert!(
      orphan.exists(),
      "a running container's responses are not ours to delete"
    );

    session
      .start(&RecordingEngine::new())
      .expect("start should succeed");
    assert!(!orphan.exists(), "a replaced container's are");
  }

  /// The same edit, once the container has actually been recreated.
  #[test]
  fn recreating_the_container_records_the_new_mounts() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let mut session = session_under(&base);
    let engine = RecordingEngine::new();

    session.start(&engine).expect("start should succeed");
    session.manifest.paths.push(PathEntry {
      local: false,
      readonly: false,
      source: base.join("vendor").display().to_string(),
      target: None,
    });
    session.restart(&engine).expect("restart should succeed");

    let recorded = Record::load(&session.mount_record())
      .expect("load should succeed")
      .expect("restart must have rewritten the record");

    assert!(
      recorded
        .drift(&session.mounts(), &session.sockets())
        .is_empty(),
      "the record must describe the container that is running now: {recorded:?}"
    );
  }

  #[test]
  fn builds_exec_spec() {
    assert_eq!(
      session().exec_spec(&["claude".to_string(), "--continue".to_string()]),
      ExecSpec {
        arguments: vec!["claude".to_string(), "--continue".to_string()],
        env: vec![
          EnvVar::Set {
            name: "CLAUDE_CONFIG_DIR".to_string(),
            value: "/home/claude/.claude".to_string(),
          },
          EnvVar::Set {
            name: "IS_SANDBOX".to_string(),
            value: "1".to_string(),
          },
        ],
        interactive: true,
        name: "compostbin-cb".to_string(),
        tty: true,
        workdir: Some(PathBuf::from("/workspace/workspace/compostbin")),
      }
    );
  }

  /// Claude copies through `wl-copy` only when a display says there is somewhere
  /// to copy to.
  #[test]
  fn names_a_display_only_for_the_clipboard() {
    let mut session = session();
    let display = EnvVar::Set {
      name: "WAYLAND_DISPLAY".to_string(),
      value: CLIPBOARD_DISPLAY.to_string(),
    };

    assert!(!session.exec_spec(&[]).env.contains(&display));

    session.manifest.host.clipboard = true;

    assert!(session.exec_spec(&[]).env.contains(&display));
    assert!(
      session
        .mounts()
        .iter()
        .any(|mount| mount.target == PathBuf::from(GUEST_SPOOL_TARGET)),
      "the clipboard is served through the spool"
    );
  }

  #[test]
  fn mounts_project_dir_when_outside_every_root() {
    let session = Session::new(
      toml::from_str("").expect("empty manifest should parse"),
      PathResolver::new("/Users/user/code/loose", "/Users/user"),
      "/Users/user/code/loose",
    );

    assert_eq!(
      session.mounts(),
      [
        Mount {
          readonly: false,
          source: "/Users/user/code/loose".into(),
          target: "/workspace/loose".into(),
        },
        Mount {
          readonly: true,
          source: "/Users/user/.local/state/compostbin/sessions/compostbin-loose/managed".into(),
          target: "/etc/claude-code".into(),
        },
        Mount {
          readonly: false,
          source: "/Users/user/.local/state/compostbin/sessions/compostbin-loose/claude-home".into(),
          target: "/home/claude/.claude".into(),
        },
      ]
    );
    assert_eq!(session.workdir(), PathBuf::from("/workspace/loose"));
  }

  #[test]
  fn skips_project_mount_when_inside_a_root() {
    let sources: Vec<String> = session()
      .mounts()
      .iter()
      .map(|mount| mount.source.display().to_string())
      .collect();

    assert_eq!(
      sources,
      [
        "/Users/user/workspace",
        "/Users/user/.cargo/registry",
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/managed",
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home",
      ]
    );
  }

  /// No allowlist means no channel at all: the spool must be absent from the
  /// argv, not merely unused.
  #[test]
  fn mounts_the_spool_only_when_commands_are_declared() {
    let spool = "/Users/user/.local/state/compostbin/sessions/compostbin-cb/host";

    assert!(
      !session()
        .mounts()
        .iter()
        .any(|mount| mount.source == PathBuf::from(spool)),
      "an undeclared channel must not be mounted: {:?}",
      session().mounts()
    );

    let mut declared = session();
    declared.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");

    assert!(
      declared.mounts().contains(&Mount {
        readonly: false,
        source: spool.into(),
        target: GUEST_SPOOL_TARGET.into(),
      }),
      "a declared command needs the spool mounted: {:?}",
      declared.mounts()
    );
  }

  /// `prepare` creates the mount source, so an undeclared channel must leave
  /// nothing behind.
  #[test]
  fn prepares_the_spool_only_when_commands_are_declared() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let mut session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );

    session
      .prepare_host_spool()
      .expect("prepare should succeed");
    assert!(!session.host_spool().exists(), "no allowlist, no spool");

    session.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");
    session
      .prepare_host_spool()
      .expect("prepare should succeed");
    assert!(session.host_spool().exists());
  }

  #[test]
  fn resolves_claude_home_on_the_host() {
    assert_eq!(
      session().claude_home(),
      PathBuf::from("/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home")
    );
  }

  #[test]
  fn builds_run_spec() {
    let sessions = "/Users/user/.local/state/compostbin/sessions/compostbin-cb";

    assert_eq!(
      session().run_spec(),
      RunSpec {
        arguments: KEEPALIVE_COMMAND.map(str::to_string).to_vec(),
        cpus: Some(4),
        detach: true,
        env: vec![EnvVar::Inherit("ANTHROPIC_API_KEY".to_string())],
        image: "compostbin/base:latest".to_string(),
        memory: Some("8G".to_string()),
        mounts: vec![
          Mount {
            readonly: false,
            source: PathBuf::from("/Users/user/workspace"),
            target: PathBuf::from("/workspace/workspace"),
          },
          Mount {
            readonly: true,
            source: PathBuf::from("/Users/user/.cargo/registry"),
            target: PathBuf::from("/workspace/registry"),
          },
          Mount {
            readonly: true,
            source: PathBuf::from(format!("{sessions}/managed")),
            target: PathBuf::from(MANAGED_SETTINGS_TARGET),
          },
          Mount {
            readonly: false,
            source: PathBuf::from(format!("{sessions}/claude-home")),
            target: PathBuf::from(CLAUDE_HOME_TARGET),
          },
        ],
        name: "compostbin-cb".to_string(),
        sockets: Vec::new(),
        workdir: Some(PathBuf::from("/workspace/workspace/compostbin")),
      }
    );
  }

  #[test]
  fn keeps_host_paths_out_of_the_container() {
    let session = session();

    let leaking: Vec<String> = session
      .mounts()
      .iter()
      .map(|mount| mount.target.display().to_string())
      .chain([session.workdir().display().to_string()])
      .filter(|guest| guest.starts_with("/Users"))
      .collect();

    assert_eq!(
      leaking,
      Vec::<String>::new(),
      "a host path may appear only as the source half of a mount"
    );
  }

  #[test]
  fn runs_the_base_image_unless_the_manifest_adds_to_it() {
    assert_eq!(session().image(), "compostbin/base:latest");

    let mut session = session();
    session.manifest.image.packages = vec!["direnv".to_string()];

    assert_eq!(session.image(), "compostbin/compostbin-cb:latest");
    assert_eq!(
      session.run_spec().image,
      "compostbin/compostbin-cb:latest",
      "the session must run the image it built"
    );
  }

  /// The relay is the container's process, so its ports are fixed at creation
  /// like the mounts; with none, the container only has to stay alive.
  #[test]
  fn runs_port_relay() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    assert_eq!(
      session.run_spec().arguments,
      [GUEST_PORTS_NAME, "7001", "7002"],
      "the relay is the container's own process"
    );
  }

  #[test]
  fn forwards_through_a_socket_in_the_session_directory() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];

    assert_eq!(
      session.forwards(),
      [Forward::to_loopback(&session.port_sockets(), 7001)]
    );
    assert_eq!(
      session.forwards()[0].listen,
      session.state_dir().join("ports").join("7001.sock")
    );
  }

  /// Relayed rather than mounted, and named after the port, since the guest's
  /// relay finds it by name.
  #[test]
  fn relays_a_socket_per_declared_port() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    let sockets = session.sockets();

    for port in [7001, 7002] {
      let source = session
        .state_dir()
        .join("ports")
        .join(format!("{port}.sock"));
      let socket = sockets
        .iter()
        .find(|socket| socket.source == source)
        .unwrap_or_else(|| panic!("port {port} should be relayed: {sockets:?}"));

      assert_eq!(
        socket.target,
        PathBuf::from(format!("/run/compostbin/ports/{port}.sock"))
      );
    }
  }

  /// A socket mounted as a filesystem is not a relay, and the framework engine
  /// would try exactly that. The two must not be confused again.
  #[test]
  fn mounts_no_socket_for_a_declared_port() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];

    assert!(
      !session
        .mounts()
        .iter()
        .any(|mount| mount.source.starts_with(session.port_sockets())),
      "a declared port is relayed, never mounted"
    );
  }

  #[test]
  fn relays_no_sockets_without_ports() {
    let session = session();

    assert!(
      session.sockets().is_empty(),
      "a session declaring no ports relays nothing"
    );
  }

  #[test]
  fn cleans_transient_state_but_keeps_the_conversation() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");
    std::fs::write(session.host_spool().join("requests").join("0001.request"), "run\n").expect("write a request");
    std::fs::create_dir_all(session.managed_settings()).expect("create managed settings");
    std::fs::write(session.managed_settings().join("session-context.txt"), "briefing").expect("write a briefing");
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    let cleared = session.clean(false).expect("clean should succeed");

    assert_eq!(cleared, [session.host_spool(), session.managed_settings()]);
    assert!(
      session.host_spool().join("requests").exists(),
      "the container may still hold this mount, and a directory it lost cannot be given back"
    );
    assert_eq!(
      std::fs::read_dir(session.host_spool().join("requests"))
        .expect("read requests")
        .count(),
      0,
      "what was in flight is gone"
    );
    assert!(
      session.managed_settings().exists(),
      "the same mount argument: the briefing is rewritten on the next create"
    );
    assert_eq!(
      std::fs::read_dir(session.managed_settings())
        .expect("read managed settings")
        .count(),
      0,
      "keeping a stale briefing would be worse than an empty directory"
    );
    assert!(
      session.claude_home().exists(),
      "the conversation `--continue` reattaches to must survive"
    );
  }

  /// The bug this is here to prevent: a spool unlinked while the container held
  /// it left every later `shell` and `host-agent` writing to an inode the guest
  /// could no longer reach, so the session had a host channel that was present
  /// and permanently empty.
  #[test]
  fn cleaning_keeps_the_inode_a_running_container_mounts() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");

    let before = std::fs::metadata(session.host_spool())
      .expect("the spool")
      .ino();
    session.clean(false).expect("clean should succeed");
    session
      .clean_after_exit()
      .expect("exit cleanup should succeed");
    let after = std::fs::metadata(session.host_spool())
      .expect("the spool")
      .ino();

    assert_eq!(before, after, "the mount source must be the same directory throughout");
  }

  #[test]
  fn exit_cleanup_keeps_managed_settings() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");
    std::fs::write(session.host_spool().join("running").join("0001"), "claimed").expect("write a claim");
    std::fs::create_dir_all(session.managed_settings()).expect("create managed settings");

    let cleared = session.clean_after_exit().expect("clean should succeed");

    assert_eq!(cleared, [session.host_spool()]);
    assert_eq!(
      std::fs::read_dir(session.host_spool().join("running"))
        .expect("read running")
        .count(),
      0,
      "nothing claimed can outlive the session that claimed it"
    );
    assert!(
      session.host_spool().join("requests").exists(),
      "the container is still holding this mount"
    );
    assert!(
      session.managed_settings().exists(),
      "the container outlives the exit, and the next `run` attaches without rewriting the briefing"
    );
  }

  #[test]
  fn cleaning_everything_takes_the_session_directory() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      [session.state_dir()]
    );
    assert!(!session.state_dir().exists());
  }

  #[test]
  fn cleaning_state_that_is_already_gone_is_not_an_error() {
    let temp = TempDir::new().expect("temp dir");
    let base = temp.path().canonicalize().expect("canonical temp");
    let session = Session::new(
      toml::from_str(MANIFEST).expect("manifest should parse"),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    );

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      Vec::<PathBuf>::new()
    );
  }
}
