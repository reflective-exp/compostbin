//! Building images, which is the one thing only the `container` CLI can do.
//!
//! Containerization manages and pulls OCI images; it does not build them. The
//! CLI's builder is BuildKit in a container of its own, and nothing in the
//! framework replaces it. So this outlives the CLI as a way of *running*
//! containers, and is deliberately not part of `Engine`: a builder is not a
//! session, and a session never builds.

use crate::error::EngineError;
use crate::model::BuildSpec;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub trait Builder {
  /// Builds an image, streaming the build log to our stdio. Returns the exit
  /// code.
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError>;

  /// Starts the builder container `build` needs, succeeding when it is already
  /// up. Sized explicitly because the 2 GB default OOMs installing Claude Code.
  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError>;
}

/// Drives the `container` CLI as a subprocess.
pub struct CliBuilder {
  program: PathBuf,
}

impl CliBuilder {
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

  /// Inherits our stdio, so a build's output reaches the terminal as it happens
  /// instead of buffering until the process ends.
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

impl Default for CliBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl Builder for CliBuilder {
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError> {
    self.passthrough(spec.to_argv())
  }

  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError> {
    let mut argv = vec!["builder".to_string(), "start".to_string()];

    if let Some(memory) = memory {
      argv.push("--memory".to_string());
      argv.push(memory.to_string());
    }

    self.capture(argv).map(|_| ())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn spec() -> BuildSpec {
    BuildSpec {
      context: "/tmp/context".into(),
      memory: None,
      tag: "base:latest".to_string(),
    }
  }

  #[test]
  fn reports_a_failed_build_by_exit_code() {
    assert_eq!(
      CliBuilder::with_program("/bin/echo")
        .build(&spec())
        .expect("echo should succeed"),
      0
    );
    assert_eq!(
      CliBuilder::with_program("/usr/bin/false")
        .build(&spec())
        .expect("false should run"),
      1
    );
  }

  #[test]
  fn reports_a_failed_builder_start_with_argv_and_stderr() {
    let error = CliBuilder::with_program("/usr/bin/false")
      .start_builder(Some("8G"))
      .expect_err("false should fail");

    assert_eq!(error.exit_code(), Some(1));
    assert!(
      error.to_string().contains("builder start --memory 8G"),
      "error should name the command: {error}"
    );
  }

  #[test]
  fn says_nothing_is_there_when_the_program_is_missing() {
    let error = CliBuilder::with_program("/nonexistent/container")
      .build(&spec())
      .expect_err("a missing program should fail");

    assert!(
      matches!(error, EngineError::Spawn { .. }),
      "a missing CLI is not a failed build: {error}"
    );
  }
}
