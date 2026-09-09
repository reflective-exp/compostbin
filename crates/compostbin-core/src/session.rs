use crate::error::PathError;
use crate::host::{GUEST_SPOOL_TARGET, Spool};
use crate::manifest::{Manifest, PathEntry, SESSIONS_DIR};
use crate::paths::{PathResolver, root_containing};
use crate::workspace::{Origin, Workspace};
use apple_container::engine::Engine;
use apple_container::error::EngineError;
use apple_container::model::{EnvVar, ExecSpec, Mount, RunSpec};
use std::path::PathBuf;

/// Where Claude's home is mounted inside the container. The container runs as
/// `claude`, matching the reference implementation.
pub const CLAUDE_HOME_TARGET: &str = "/home/claude/.claude";
/// Under the session state directory: kept, because `claude --continue` reads it.
pub const CLAUDE_HOME_DIR: &str = "claude-home";
/// Under the session state directory: removed by `clean`, because it holds only
/// requests in flight.
pub const SPOOL_DIR: &str = "host";
/// Keeps a detached container alive so `exec` has something to attach to.
pub const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "infinity"];
pub const NAME_PREFIX: &str = "compostbin-";

/// What `add` did, and therefore what the caller must do next.
#[derive(Clone, Debug, PartialEq)]
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
  /// `path` must already be canonical: a symlink pointing out of a root looks
  /// contained but is not, in the container.
  pub fn add(&mut self, path: impl Into<PathBuf>, readonly: bool) -> AddOutcome {
    let path = path.into();

    if let Some(root) = root_containing(&path, &self.resolved_roots()) {
      return AddOutcome::AlreadyMounted {
        root: root.to_path_buf(),
      };
    }

    self.manifest.paths.push(PathEntry {
      readonly,
      source: path.display().to_string(),
      target: None,
    });

    AddOutcome::NeedsRestart
  }

  /// Resolves a manifest path string against this session's working directory
  /// and home, so callers need no `PathResolver` of their own.
  pub fn resolve(&self, raw: &str) -> PathBuf {
    self.resolver.resolve(raw)
  }

  pub fn resolver(&self) -> &PathResolver {
    &self.resolver
  }

  /// Claude's home on the host — the source side of the bind mount, and so the
  /// place to seed credentials before the container starts.
  ///
  /// Per session unless the manifest overrides it: one shared home would mean
  /// every project's `--continue` resumed whichever project ran last, and one
  /// project's history would be readable from another project's container.
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

  /// What the session may delete on exit: state that means nothing once the
  /// container is gone. Claude's home is deliberately not here — losing it
  /// would lose the conversation `--continue` reattaches to.
  pub fn transient_state(&self) -> Vec<PathBuf> {
    vec![self.host_spool()]
  }

  /// Removes this session's transient state, and with `everything` the session
  /// directory whole — Claude's home included. Missing paths are not an error:
  /// `clean` exists precisely because an earlier exit may not have run.
  pub fn clean(&self, everything: bool) -> Result<Vec<PathBuf>, PathError> {
    let targets = if everything {
      vec![self.state_dir()]
    } else {
      self.transient_state()
    };

    let mut removed = Vec::new();
    for target in targets {
      if !target.exists() {
        continue;
      }
      std::fs::remove_dir_all(&target).map_err(|source| PathError::new(&target, source))?;
      removed.push(target);
    }

    Ok(removed)
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

  /// The host paths this session exposes, in declaration order: roots, then the
  /// project directory when no root covers it, then explicit `[[paths]]`. Order
  /// matters — a name is taken by the first entry that claims it.
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
      workspace.push(
        self.resolver.resolve(&entry.source),
        target,
        Origin::Explicit,
        entry.readonly,
      );
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
    if !self.manifest.host.is_empty() {
      mounts.push(Mount {
        readonly: false,
        source: self.host_spool(),
        target: PathBuf::from(GUEST_SPOOL_TARGET),
      });
    }

    mounts.push(Mount {
      readonly: false,
      source: self.claude_home(),
      target: PathBuf::from(CLAUDE_HOME_TARGET),
    });

    mounts
  }

  /// Where the session lands inside the container: the project directory's own
  /// `/workspace` path. Falling back to `/workspace` itself keeps a
  /// misconfigured manifest from starting a container in a directory that is not
  /// mounted at all.
  pub fn workdir(&self) -> PathBuf {
    self
      .workspace()
      .guest_path(&self.project_dir)
      .unwrap_or_else(|| PathBuf::from(crate::workspace::WORKSPACE_TARGET))
  }

  /// Inside the session directory, so concurrent projects cannot see each
  /// other's requests and `clean` takes it away with the rest.
  pub fn host_spool(&self) -> PathBuf {
    self.state_dir().join(SPOOL_DIR)
  }

  /// The image the session runs: a derived one when the manifest adds packages
  /// or build steps, otherwise the shared base itself.
  pub fn image(&self) -> String {
    if self.manifest.image.is_empty() {
      self.manifest.project.image.clone()
    } else {
      format!("compostbin/{}:latest", self.container_name())
    }
  }

  pub fn run_spec(&self) -> RunSpec {
    RunSpec {
      arguments: KEEPALIVE_COMMAND
        .iter()
        .map(|word| word.to_string())
        .collect(),
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
      workdir: Some(self.workdir()),
    }
  }

  /// The mount source must exist before the container starts, and the guest
  /// cannot create it. A no-op when no commands are declared.
  pub fn prepare_host_spool(&self) -> Result<(), PathError> {
    if self.manifest.host.is_empty() {
      return Ok(());
    }

    Spool::new(self.host_spool()).create()
  }

  /// Makes the session's container exist and be running, doing nothing when it
  /// already is: `container run` refuses a name that is taken, so a second `run`
  /// must attach rather than recreate. A container that exists but has stopped
  /// is deleted and recreated, since its mounts may no longer match the manifest.
  pub fn start(&self, engine: &impl Engine) -> Result<(), EngineError> {
    let name = self.container_name();

    if engine.running_containers()?.contains(&name) {
      return Ok(());
    }

    if engine.containers()?.contains(&name) {
      engine.delete(&name)?;
    }

    engine.run(&self.run_spec()).map(|_| ())
  }

  /// Recreates the container so a new mount takes effect — mounts cannot be added
  /// to a running container — then reattaches to the same Claude conversation.
  /// Cheap because Claude's home is a bind mount that outlives the container.
  pub fn restart(&self, engine: &impl Engine) -> Result<i32, EngineError> {
    let name = self.container_name();
    engine.stop(&name)?;
    engine.delete(&name)?;
    engine.run(&self.run_spec())?;
    engine.exec(&self.exec_spec(&["claude".to_string(), "--continue".to_string()]))
  }

  /// `IS_SANDBOX=1` is set on the process rather than the container, matching the
  /// reference implementation: it tells Claude it is already sandboxed.
  ///
  /// `CLAUDE_CONFIG_DIR` moves `.claude.json` — the account, the onboarding
  /// answers, and the per-project trust — into the bind-mounted home. It lives at
  /// `~/.claude.json` by default, outside the mount, so a new container starts
  /// logged out however faithfully `~/.claude` is preserved.
  pub fn exec_spec(&self, arguments: &[String]) -> ExecSpec {
    ExecSpec {
      arguments: arguments.to_vec(),
      env: vec![
        EnvVar::Set {
          name: "CLAUDE_CONFIG_DIR".to_string(),
          value: CLAUDE_HOME_TARGET.to_string(),
        },
        EnvVar::Set {
          name: "IS_SANDBOX".to_string(),
          value: "1".to_string(),
        },
      ],
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
  use apple_container::fake::RecordingEngine;
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

  #[test]
  fn add_inside_a_root_changes_nothing() {
    let mut session = session();

    let outcome = session.add("/Users/user/workspace/other", false);

    assert_eq!(
      outcome,
      AddOutcome::AlreadyMounted {
        root: "/Users/user/workspace".into()
      }
    );
    assert_eq!(session.manifest.paths.len(), 1, "no new [[paths]] entry");
    assert_eq!(session.mounts().len(), 3, "no new mount");
  }

  #[test]
  fn add_outside_every_root_records_a_path() {
    let mut session = session();

    let outcome = session.add("/Users/user/vendor/libfoo", true);

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
  }

  #[test]
  fn start_attaches_to_an_already_running_container() {
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", true)]);

    session().start(&engine).expect("start should succeed");

    assert_eq!(
      engine.calls(),
      [vec!["ls", "--quiet"]],
      "a running container must not be recreated"
    );
  }

  #[test]
  fn start_recreates_a_stopped_container() {
    let engine = RecordingEngine::with_containers(&[("compostbin-cb", false)]);

    session().start(&engine).expect("start should succeed");

    let calls = engine.calls();
    assert_eq!(calls[0], ["ls", "--quiet"]);
    assert_eq!(calls[1], ["ls", "--all", "--quiet"]);
    assert_eq!(calls[2], ["delete", "compostbin-cb"]);
    assert_eq!(calls[3], session().run_spec().to_argv());
    assert_eq!(calls.len(), 4);
  }

  #[test]
  fn start_creates_a_container_that_does_not_exist() {
    let engine = RecordingEngine::with_containers(&[("compostbin-other", true)]);

    session().start(&engine).expect("start should succeed");

    let calls = engine.calls();
    assert_eq!(calls[0], ["ls", "--quiet"]);
    assert_eq!(calls[1], ["ls", "--all", "--quiet"]);
    assert_eq!(calls[2], session().run_spec().to_argv());
    assert_eq!(calls.len(), 3, "nothing to delete: {calls:?}");
  }

  #[test]
  fn restarts_by_stopping_deleting_and_continuing() {
    let engine = RecordingEngine::new();

    session().restart(&engine).expect("restart should succeed");

    let calls = engine.calls();
    assert_eq!(calls[0], ["stop", "compostbin-cb"]);
    assert_eq!(calls[1], ["delete", "compostbin-cb"]);
    assert_eq!(calls[2], session().run_spec().to_argv());
    assert_eq!(
      calls[3],
      session()
        .exec_spec(&["claude".to_string(), "--continue".to_string()])
        .to_argv()
    );
    assert_eq!(calls.len(), 4);
  }

  #[test]
  fn preserves_claude_home_across_restart() {
    let engine = RecordingEngine::new();

    session().restart(&engine).expect("restart should succeed");

    let run = &engine.calls()[2];
    assert!(
      run.contains(
        &"/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home:/home/claude/.claude".to_string()
      ),
      "the recreated container must remount Claude's home: {run:?}"
    );
  }

  #[test]
  fn builds_exec_spec() {
    assert_eq!(
      session()
        .exec_spec(&["claude".to_string(), "--continue".to_string()])
        .to_argv(),
      [
        "exec",
        "--env",
        "CLAUDE_CONFIG_DIR=/home/claude/.claude",
        "--env",
        "IS_SANDBOX=1",
        "--interactive",
        "--tty",
        "--workdir",
        "/workspace/workspace/compostbin",
        "compostbin-cb",
        "claude",
        "--continue",
      ]
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
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home",
      ]
    );
  }

  /// D6: with no allowlist there is no channel at all, rather than an empty
  /// one — so the spool must be absent from the argv, not merely unused.
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

  /// The mount source has to exist before `run`, and only then: `prepare` is
  /// what creates it, so an undeclared channel must leave nothing behind.
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
    assert_eq!(
      session().run_spec().to_argv(),
      [
        "run",
        "--cpus",
        "4",
        "--detach",
        "--env",
        "ANTHROPIC_API_KEY",
        "--memory",
        "8G",
        "--name",
        "compostbin-cb",
        "--volume",
        "/Users/user/workspace:/workspace/workspace",
        "--volume",
        "/Users/user/.cargo/registry:/workspace/registry:ro",
        "--volume",
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home:/home/claude/.claude",
        "--workdir",
        "/workspace/workspace/compostbin",
        "compostbin/base:latest",
        "sleep",
        "infinity",
      ]
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
    assert!(
      session
        .run_spec()
        .to_argv()
        .contains(&"compostbin/compostbin-cb:latest".to_string()),
      "the session must run the image it built"
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
    std::fs::create_dir_all(session.host_spool()).expect("create spool");
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    let removed = session.clean(false).expect("clean should succeed");

    assert_eq!(removed, [session.host_spool()]);
    assert!(!session.host_spool().exists());
    assert!(
      session.claude_home().exists(),
      "the conversation `--continue` reattaches to must survive"
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
