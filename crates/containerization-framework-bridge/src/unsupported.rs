//! Non-macOS engine and builder: the same surface as the macOS ones, failing on
//! first use of anything that needs a VM, so the workspace compiles on Linux.
//!
//! Every method the macOS types have belongs here too, or the crates above
//! stop building on Linux while still building on macOS.

use crate::store::Store;
use compostbin_engine::builder::Builder;
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{BuildPlan, ExecSpec, RunSpec};
use std::io;
use std::path::PathBuf;

fn unsupported() -> EngineError {
  EngineError::unavailable("use Containerization.framework", "it is macOS only")
}

pub struct FrameworkEngine {
  store: Store,
}

impl FrameworkEngine {
  pub fn new(_runtime_dir: impl Into<PathBuf>, store: Store) -> Self {
    Self { store }
  }

  /// Nothing here attaches, so nothing ever breaks off.
  pub fn reporting(self, _report: impl Fn(io::Error) + Send + Sync + 'static) -> Self {
    self
  }
}

impl Engine for FrameworkEngine {
  fn exec(&self, _spec: &ExecSpec) -> Result<i32, EngineError> {
    Err(unsupported())
  }

  /// The store is plain files, so this answers on Linux as it does on macOS.
  fn images(&self) -> Result<Vec<String>, EngineError> {
    self
      .store
      .images()
      .map_err(|error| EngineError::unavailable("read the image index", error))
  }

  fn is_running(&self, _name: &str) -> Result<bool, EngineError> {
    Err(unsupported())
  }

  fn is_unpacked(&self, _image: &str) -> Result<bool, EngineError> {
    Err(unsupported())
  }

  fn run(&self, _spec: &RunSpec) -> Result<(), EngineError> {
    Err(unsupported())
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    Err(unsupported())
  }
}

pub struct FrameworkBuilder;

impl FrameworkBuilder {
  pub fn new(_store: Store) -> Self {
    Self
  }
}

impl Builder for FrameworkBuilder {
  fn build(&self, _plan: &BuildPlan) -> Result<(), EngineError> {
    Err(unsupported())
  }

  fn provision(&self) -> Result<(), EngineError> {
    Err(unsupported())
  }
}
