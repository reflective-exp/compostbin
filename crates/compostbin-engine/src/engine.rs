//! A running container, whoever is running it.
//!
//! Deliberately not image building: that is `builder`. A session never builds,
//! and a build is not a session.

use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};

pub trait Engine {
  /// Runs a command in a live container, attaching it to the terminal so an
  /// interactive session keeps the real one.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Every image available to run, as `name:tag`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  /// Starts a container and returns its id.
  fn run(&self, spec: &RunSpec) -> Result<String, EngineError>;

  /// The containers that are running, and so ready for `exec`. A container that
  /// is not running does not exist: it dies with the process that started it.
  fn running_containers(&self) -> Result<Vec<String>, EngineError>;

  /// What is running containers, and at what version. `None` when it cannot
  /// say, which `doctor` reports rather than treating as a failure.
  fn version(&self) -> Result<Option<String>, EngineError>;
}
