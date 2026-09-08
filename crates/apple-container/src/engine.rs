use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub trait Engine {
  fn delete(&self, name: &str) -> Result<(), EngineError>;

  /// Runs a command in a live container, inheriting our stdio so an interactive
  /// session keeps the real terminal. Returns the process exit code.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Starts a container and returns its id.
  fn run(&self, spec: &RunSpec) -> Result<String, EngineError>;

  fn stop(&self, name: &str) -> Result<(), EngineError>;
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
}

impl Default for CliEngine {
  fn default() -> Self {
    Self::new()
  }
}

impl Engine for CliEngine {
  fn delete(&self, name: &str) -> Result<(), EngineError> {
    self
      .capture(vec!["delete".to_string(), name.to_string()])
      .map(|_| ())
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    let argv = spec.to_argv();
    let status = Command::new(&self.program)
      .args(&argv)
      .stdin(Stdio::inherit())
      .stdout(Stdio::inherit())
      .stderr(Stdio::inherit())
      .status()
      .map_err(|source| EngineError::Spawn { argv, source })?;

    Ok(status.code().unwrap_or(-1))
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.capture(spec.to_argv())
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self
      .capture(vec!["stop".to_string(), name.to_string()])
      .map(|_| ())
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
