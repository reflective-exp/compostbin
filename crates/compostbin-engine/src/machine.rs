use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};
use std::os::fd::OwnedFd;

pub trait Machine {
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError>;

  /// Only works in the process that ran the container.
  fn listen(&self, name: &str, port: u32) -> Result<Box<dyn Listener>, EngineError>;

  /// A container exists only while the process that started it is alive.
  fn is_running(&self, name: &str) -> bool;

  fn version(&self) -> String;
}

pub trait Listener: Send + Sync {
  /// Returns `None` once `finish` has been called.
  fn accept(&self) -> Option<OwnedFd>;

  /// Unblocks every pending and future `accept`; idempotent.
  fn finish(&self);
}
