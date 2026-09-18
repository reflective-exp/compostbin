//! Running containers. Image building is `builder`.

use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};

pub trait Engine {
  /// Runs a command in a live container, attached to the real terminal.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Every image available to run, as `name:tag`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  /// Starts a container named `spec.name`.
  fn run(&self, spec: &RunSpec) -> Result<(), EngineError>;

  /// Whether this container is running, and so ready for `exec`. A stopped
  /// container doesn't exist: it dies with the process that started it.
  fn is_running(&self, name: &str) -> Result<bool, EngineError>;

  /// Whether `run` can skip unpacking this image. False until its first
  /// (slow) run.
  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError>;

  /// The engine and its version. `None` when unknown, which `doctor` reports
  /// rather than treating as a failure.
  fn version(&self) -> Result<Option<String>, EngineError>;
}
