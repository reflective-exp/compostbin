//! The engine and the builder off macOS: the same names and constructors,
//! failing at the first thing asked of them.
//!
//! compostbin only runs on macOS, but it has to compile inside its own Debian
//! guest, where `cargo check` is how a session checks its work.

use crate::store::Store;
use compostbin_engine::builder::Builder;
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{BuildPlan, ExecSpec, RunSpec};
use std::path::PathBuf;

fn unsupported() -> EngineError {
  EngineError::unavailable("use Containerization.framework", "it is macOS only")
}

pub struct FrameworkEngine;

impl FrameworkEngine {
  pub fn new(_runtime_dir: impl Into<PathBuf>, _store: Store) -> Self {
    Self
  }
}

impl Engine for FrameworkEngine {
  fn exec(&self, _spec: &ExecSpec) -> Result<i32, EngineError> {
    Err(unsupported())
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    Err(unsupported())
  }

  fn is_running(&self, _name: &str) -> Result<bool, EngineError> {
    Err(unsupported())
  }

  fn is_unpacked(&self, _image: &str) -> Result<bool, EngineError> {
    Err(unsupported())
  }

  fn run(&self, _spec: &RunSpec) -> Result<String, EngineError> {
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

  pub fn provision(&self) -> Result<(), EngineError> {
    Err(unsupported())
  }
}

impl Builder for FrameworkBuilder {
  fn build(&self, _plan: &BuildPlan) -> Result<(), EngineError> {
    Err(unsupported())
  }
}
