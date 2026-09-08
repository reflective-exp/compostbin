use crate::manifest::Manifest;
use crate::paths::{PathResolver, root_containing};
use apple_container::engine::Engine;
use apple_container::error::EngineError;
use apple_container::model::{EnvVar, ExecSpec, Mount, RunSpec};
use std::path::PathBuf;

/// Where Claude's home is mounted inside the container. The container runs as
/// root, matching the reference implementation.
pub const CLAUDE_HOME_TARGET: &str = "/root/.claude";
/// Keeps a detached container alive so `exec` has something to attach to.
pub const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "infinity"];
pub const NAME_PREFIX: &str = "compostbin-";

pub struct Session {
  manifest: Manifest,
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

    mounts.push(Mount {
      readonly: false,
      source: self.resolver.resolve(&self.manifest.claude.home),
      target: PathBuf::from(CLAUDE_HOME_TARGET),
    });

    mounts
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
  pub fn exec_spec(&self, arguments: &[String]) -> ExecSpec {
    ExecSpec {
      arguments: arguments.to_vec(),
      env: vec![EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      }],
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
      PathResolver::new("/Users/sax/workspace/compostbin", "/Users/sax"),
      "/Users/sax/workspace/compostbin",
    )
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
      run.contains(&"/Users/sax/.compostbin/claude-home:/root/.claude".to_string()),
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
        "IS_SANDBOX=1",
        "--interactive",
        "--tty",
        "--workdir",
        "/Users/sax/workspace/compostbin",
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
      PathResolver::new("/Users/sax/code/loose", "/Users/sax"),
      "/Users/sax/code/loose",
    );

    assert_eq!(
      session.mounts(),
      [
        Mount {
          readonly: false,
          source: "/Users/sax/code/loose".into(),
          target: "/Users/sax/code/loose".into(),
        },
        Mount {
          readonly: false,
          source: "/Users/sax/.compostbin/claude-home".into(),
          target: "/root/.claude".into(),
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
        "/Users/sax/workspace",
        "/Users/sax/.cargo/registry",
        "/Users/sax/.compostbin/claude-home",
      ]
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
        "/Users/sax/workspace:/Users/sax/workspace",
        "--volume",
        "/Users/sax/.cargo/registry:/Users/sax/.cargo/registry:ro",
        "--volume",
        "/Users/sax/.compostbin/claude-home:/root/.claude",
        "--workdir",
        "/Users/sax/workspace/compostbin",
        "compostbin/base:latest",
        "sleep",
        "infinity",
      ]
    );
  }
}
