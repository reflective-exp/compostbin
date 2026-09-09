use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::io;
use std::process::ExitStatus;

/// A failed `container` invocation. Carries the argv and captured stderr: a bare
/// exit status says nothing about what to fix.
#[derive(Debug)]
pub enum EngineError {
  /// The command ran and exited non-zero.
  Failed {
    argv: Vec<String>,
    status: ExitStatus,
    stderr: String,
  },
  /// The command could not be spawned at all.
  Spawn { argv: Vec<String>, source: io::Error },
}

impl EngineError {
  pub fn argv(&self) -> &[String] {
    match self {
      Self::Failed { argv, .. } | Self::Spawn { argv, .. } => argv,
    }
  }

  pub fn exit_code(&self) -> Option<i32> {
    match self {
      Self::Failed { status, .. } => status.code(),
      Self::Spawn { .. } => None,
    }
  }
}

impl Display for EngineError {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
    let command = self.argv().join(" ");
    match self {
      Self::Failed { status, stderr, .. } => {
        write!(formatter, "`container {command}` failed ({status})")?;
        if !stderr.trim().is_empty() {
          write!(formatter, ": {}", stderr.trim())?;
        }
        Ok(())
      }
      Self::Spawn { source, .. } => write!(formatter, "could not run `container {command}`: {source}"),
    }
  }
}

impl Error for EngineError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Failed { .. } => None,
      Self::Spawn { source, .. } => Some(source),
    }
  }
}
