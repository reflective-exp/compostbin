use crate::error::EngineError;
use crate::model::{BuildSpec, ExecSpec, RunSpec};
use crate::parse;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub trait Engine {
  /// Builds an image, streaming the build log to our stdio. Returns the exit code.
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError>;

  /// Every container the daemon knows, running or not.
  fn containers(&self) -> Result<Vec<String>, EngineError>;

  fn delete(&self, name: &str) -> Result<(), EngineError>;

  /// Runs a command in a live container, inheriting our stdio so an interactive
  /// session keeps the real terminal.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Every image the daemon holds, as `name:tag`. Fails when the daemon is not
  /// running — plugins are unavailable until `container system start`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  /// Starts a container and returns its id.
  fn run(&self, spec: &RunSpec) -> Result<String, EngineError>;

  /// Only the containers that are running, and so ready for `exec`.
  fn running_containers(&self) -> Result<Vec<String>, EngineError>;

  /// Starts the builder container `build` needs, succeeding when it is already
  /// up. Sized explicitly because the 2 GB default OOMs installing Claude Code.
  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError>;

  fn stop(&self, name: &str) -> Result<(), EngineError>;

  /// The CLI's own version. `None` when the output does not look like a version,
  /// which `doctor` reports rather than treating as a failure.
  fn version(&self) -> Result<Option<String>, EngineError>;
}

/// Drives the `container` CLI as a subprocess.
pub struct CliEngine {
  program: PathBuf,
}

impl CliEngine {
  pub fn new() -> Self {
    Self::with_program("container")
  }

  pub fn with_program(program: impl Into<PathBuf>) -> Self {
    Self {
      program: program.into(),
    }
  }

  fn capture(&self, argv: Vec<String>) -> Result<String, EngineError> {
    let output = Command::new(&self.program)
      .args(&argv)
      .output()
      .map_err(|source| EngineError::Spawn {
        argv: argv.clone(),
        source,
      })?;

    if !output.status.success() {
      return Err(EngineError::Failed {
        argv,
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
      });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
  }

  fn container_names(&self, argv: &[&str]) -> Result<Vec<String>, EngineError> {
    let stdout = self.capture(argv.iter().map(|word| word.to_string()).collect())?;

    Ok(parse::names(&stdout))
  }

  /// Inherits our stdio, so long-running output reaches the terminal as it
  /// happens instead of buffering until the process ends.
  fn passthrough(&self, argv: Vec<String>) -> Result<i32, EngineError> {
    let status = Command::new(&self.program)
      .args(&argv)
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit())
      .stderr(Stdio::inherit())
      .status()
      .map_err(|source| EngineError::Spawn { argv, source })?;

    Ok(status.code().unwrap_or(-1))
  }
}

impl Default for CliEngine {
  fn default() -> Self {
    Self::new()
  }
}

impl Engine for CliEngine {
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError> {
    self.passthrough(spec.to_argv())
  }

  fn containers(&self) -> Result<Vec<String>, EngineError> {
    self.container_names(&["ls", "--all", "--quiet"])
  }

  fn delete(&self, name: &str) -> Result<(), EngineError> {
    self
      .capture(vec!["delete".to_string(), name.to_string()])
      .map(|_| ())
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.passthrough(spec.to_argv())
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    let stdout = self.capture(
      ["image", "ls", "--quiet"]
        .iter()
        .map(|word| word.to_string())
        .collect(),
    )?;

    Ok(parse::names(&stdout))
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.capture(spec.to_argv())
  }

  fn running_containers(&self) -> Result<Vec<String>, EngineError> {
    self.container_names(&["ls", "--quiet"])
  }

  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError> {
    let mut argv = vec!["builder".to_string(), "start".to_string()];

    if let Some(memory) = memory {
      argv.push("--memory".to_string());
      argv.push(memory.to_string());
    }

    self.capture(argv).map(|_| ())
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self
      .capture(vec!["stop".to_string(), name.to_string()])
      .map(|_| ())
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    let stdout = self.capture(vec!["--version".to_string()])?;

    Ok(parse::cli_version(&stdout))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn spec() -> RunSpec {
    RunSpec {
      arguments: Vec::new(),
      cpus: None,
      detach: false,
      env: Vec::new(),
      image: "base:latest".to_string(),
      memory: None,
      mounts: Vec::new(),
      name: "cb-test".to_string(),
      workdir: None,
    }
  }

  #[test]
  fn reports_a_failed_build_by_exit_code() {
    let spec = BuildSpec {
      context: "/tmp/context".into(),
      memory: None,
      tag: "base:latest".to_string(),
    };

    assert_eq!(
      CliEngine::with_program("/bin/echo")
        .build(&spec)
        .expect("echo should succeed"),
      0
    );
    assert_eq!(
      CliEngine::with_program("/usr/bin/false")
        .build(&spec)
        .expect("false should run"),
      1
    );
  }

  #[test]
  fn lists_images_by_name() {
    let engine = CliEngine::with_program("/bin/echo");

    assert_eq!(engine.images().expect("echo should succeed"), ["image ls --quiet"]);
  }

  #[test]
  fn reads_no_version_from_unexpected_output() {
    let engine = CliEngine::with_program("/bin/echo");

    assert_eq!(engine.version().expect("echo should succeed"), None);
  }

  #[test]
  fn runs_and_returns_container_id() {
    let engine = CliEngine::with_program("/bin/echo");

    assert_eq!(
      engine.run(&spec()).expect("echo should succeed"),
      "run --name cb-test base:latest"
    );
  }

  #[test]
  fn reports_failure_with_argv_and_stderr() {
    let engine = CliEngine::with_program("/usr/bin/false");

    let error = engine.run(&spec()).expect_err("false should fail");

    assert_eq!(error.exit_code(), Some(1));
    assert!(
      error.to_string().contains("run --name cb-test base:latest"),
      "error should name the command: {error}"
    );
  }
}
