//! Running containers. Image building is `builder`.

use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};
use std::os::fd::OwnedFd;

pub trait Engine {
  /// Runs a command in a live container, attached to the real terminal.
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError>;

  /// Every image available to run, as `name:tag`.
  fn images(&self) -> Result<Vec<String>, EngineError>;

  /// Starts a container named `spec.name`.
  fn run(&self, spec: &RunSpec) -> Result<(), EngineError>;

  /// Takes the connections the guest of `name` opens to vsock `port` on the
  /// host. Only the process that ran the container can: the VM is its.
  fn listen(&self, name: &str, port: u32) -> Result<Box<dyn Listener>, EngineError>;

  /// Whether this container is running, and so ready for `exec`. A stopped
  /// container doesn't exist: it dies with the process that started it.
  fn is_running(&self, name: &str) -> bool;

  /// Whether `run` can skip unpacking this image. False until its first
  /// (slow) run.
  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError>;

  /// The first blob `image` is made of that the store no longer holds, as
  /// `sha256:<hex>`; `None` when every one is there.
  fn missing_content(&self, image: &str) -> Result<Option<String>, EngineError>;

  /// The engine and its version.
  fn version(&self) -> String;
}

/// One vsock port the guest connects out to.
pub trait Listener: Send + Sync {
  /// Waits for the guest's next connection, a stream socket. `None` once
  /// `finish` has been called.
  fn accept(&self) -> Option<OwnedFd>;

  /// Ends every `accept`, waiting or to come. Calling it again does nothing.
  fn finish(&self);
}
