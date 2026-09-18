//! A running container, whoever is running it.
//!
//! Deliberately not image building: that is `builder`. A session never builds,
//! and a build is not a session.

use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};

pub trait Engine {
  /// Every container the engine knows, running or not.
  fn containers(&self) -> Result<Vec<String>, EngineError>;

  fn delete(&self, name: &str) -> Result<(), EngineError>;

  /// Runs a command in a live container, attaching it to the terminal so an
  /// interactive session keeps the real one.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Every image available to run, as `name:tag`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  /// Starts a container and returns its id.
  fn run(&self, spec: &RunSpec) -> Result<String, EngineError>;

  /// Only the containers that are running, and so ready for `exec`.
  fn running_containers(&self) -> Result<Vec<String>, EngineError>;

  fn stop(&self, name: &str) -> Result<(), EngineError>;

  /// What is running containers, and at what version. `None` when it cannot
  /// say, which `doctor` reports rather than treating as a failure.
  fn version(&self) -> Result<Option<String>, EngineError>;
}
