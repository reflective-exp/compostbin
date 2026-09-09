use crate::error::PathError;
use crate::host::{GUEST_SPOOL_TARGET, HOST_SPOOL, Spool};
use crate::manifest::{Manifest, PathEntry};
use crate::paths::{PathResolver, root_containing};
use apple_container::engine::Engine;
use apple_container::error::EngineError;
use apple_container::model::{EnvVar, ExecSpec, Mount, RunSpec};
use std::path::PathBuf;

/// Where Claude's home is mounted inside the container. The container runs as
/// `claude`, matching the reference implementation.
pub const CLAUDE_HOME_TARGET: &str = "/home/claude/.claude";
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

  /// Claude's home on the host — the source side of the bind mount, and so the
  /// place to seed credentials before the container starts.
  pub fn claude_home(&self) -> PathBuf {
    self.resolver.resolve(&self.manifest.claude.home)
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

  /// Workspace roots first, then explicit paths, then Claude's home. Order is
  /// preserved so a nested mount declared later lands on top of its parent.
  pub fn mounts(&self) -> Vec<Mount> {
    let mut mounts: Vec<Mount> = self
      .manifest
      .workspace
      .roots
      .iter()
      .map(|root| self.identical_mount(root, false))
      .collect();

    if root_containing(&self.project_dir, &self.resolved_roots()).is_none() {
      mounts.push(Mount {
        readonly: false,
        source: self.project_dir.clone(),
        target: self.project_dir.clone(),
      });
    }

    for entry in &self.manifest.paths {
      let source = self.resolver.resolve(&entry.source);
      let target = match &entry.target {
        Some(target) => self.resolver.resolve(target),
        None => source.clone(),
      };
      mounts.push(Mount {
        readonly: entry.readonly,
        source,
        target,
      });
    }

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

  /// Per container name, so concurrent projects cannot see each other's
  /// requests.
  pub fn host_spool(&self) -> PathBuf {
    self
      .resolver
      .resolve(HOST_SPOOL)
      .join(self.container_name())
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
      image: self.manifest.project.image.clone(),
      memory: Some(self.manifest.container.memory.clone()),
      mounts: self.mounts(),
      name: self.container_name(),
      workdir: Some(self.project_dir.clone()),
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
      workdir: Some(self.project_dir.clone()),
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

  fn identical_mount(&self, raw: &str, readonly: bool) -> Mount {
    let source = self.resolver.resolve(raw);
    Mount {
      readonly,
      target: source.clone(),
      source,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use apple_container::fake::RecordingEngine;

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
        target: "/Users/user/vendor/libfoo".into(),
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
      run.contains(&"/Users/user/.local/state/compostbin/claude-home:/home/claude/.claude".to_string()),
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
        "/Users/user/workspace/compostbin",
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
          target: "/Users/user/code/loose".into(),
        },
        Mount {
          readonly: false,
          source: "/Users/user/.local/state/compostbin/claude-home".into(),
          target: "/home/claude/.claude".into(),
        },
      ]
    );
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
        "/Users/user/.local/state/compostbin/claude-home",
      ]
    );
  }

  #[test]
  fn resolves_claude_home_on_the_host() {
    assert_eq!(
      session().claude_home(),
      PathBuf::from("/Users/user/.local/state/compostbin/claude-home")
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
        "/Users/user/workspace:/Users/user/workspace",
        "--volume",
        "/Users/user/.cargo/registry:/Users/user/.cargo/registry:ro",
        "--volume",
        "/Users/user/.local/state/compostbin/claude-home:/home/claude/.claude",
        "--workdir",
        "/Users/user/workspace/compostbin",
        "compostbin/base:latest",
        "sleep",
        "infinity",
      ]
    );
  }
}
