use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};
use std::os::fd::OwnedFd;

pub trait Engine {
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Each image as `name:tag`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError>;

  /// Only works in the process that ran the container.
  fn listen(&self, name: &str, port: u32) -> Result<Box<dyn Listener>, EngineError>;

  /// A container exists only while the process that started it is alive.
  fn is_running(&self, name: &str) -> bool;

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError>;

  /// The first blob of `image` missing from the store, as `sha256:<hex>`.
  fn missing_content(&self, image: &str) -> Result<Option<String>, EngineError>;

  fn version(&self) -> String;
}

pub trait Listener: Send + Sync {
  /// Returns `None` once `finish` has been called.
  fn accept(&self) -> Option<OwnedFd>;

  /// Unblocks every pending and future `accept`; idempotent.
  fn finish(&self);
}
