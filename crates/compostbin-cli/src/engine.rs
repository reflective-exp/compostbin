//! Which engine a session runs on.
//!
//! `COMPOSTBIN_ENGINE=framework` drives the session through
//! Containerization.framework instead of the `container` CLI. An environment
//! variable rather than a manifest key on purpose: the framework path is an
//! experiment, and a manifest key is a promise to keep reading it.
//!
//! The choice is per-process, and `shell` has to make the same one `run` made —
//! a `shell` on the CLI engine looks for a daemon container that a framework
//! `run` never created.

use apple_container::engine::{CliEngine, Engine};
use apple_container::error::EngineError;
use apple_container::model::{BuildSpec, ExecSpec, RunSpec};
use compostbin_core::session::Session;
use std::error::Error;

pub const ENGINE_VARIABLE: &str = "COMPOSTBIN_ENGINE";
const FRAMEWORK: &str = "framework";

/// Dispatches to whichever engine the environment asked for.
///
/// Delegation rather than `dyn Engine`, because `Engine`'s callers take
/// `&impl Engine` and a trait object does not satisfy that.
pub enum Selected {
  Cli(CliEngine),
  #[cfg(target_os = "macos")]
  Framework(Box<containerization_framework_bridge::FrameworkEngine>),
}

/// Whether the framework engine was asked for. Read once per process.
pub fn framework_selected() -> bool {
  std::env::var(ENGINE_VARIABLE).as_deref() == Ok(FRAMEWORK)
}

#[cfg(target_os = "macos")]
pub fn select(session: &Session) -> Result<Selected, Box<dyn Error>> {
  if !framework_selected() {
    return Ok(Selected::Cli(CliEngine::new()));
  }

  let store = containerization_framework_bridge::Store::discover()?;

  // The control socket lives here, so the directory has to exist before `run`
  // binds it — earlier than anything else would have created it.
  std::fs::create_dir_all(session.state_dir())?;

  Ok(Selected::Framework(Box::new(
    containerization_framework_bridge::FrameworkEngine::new(session.state_dir(), store),
  )))
}

/// compostbin only runs on macOS, but it has to compile inside its own Debian
/// guest — where `cargo check` is how a session checks its work.
#[cfg(not(target_os = "macos"))]
pub fn select(_session: &Session) -> Result<Selected, Box<dyn Error>> {
  if framework_selected() {
    return Err("Containerization.framework is macOS only".into());
  }

  Ok(Selected::Cli(CliEngine::new()))
}

/// Delegates every call to the selected engine.
///
/// Written out rather than generated: ten methods is less machinery than a
/// macro that hides which engine answers what.
macro_rules! dispatch {
  ($self:ident, $engine:ident => $call:expr) => {
    match $self {
      Selected::Cli($engine) => $call,
      #[cfg(target_os = "macos")]
      Selected::Framework($engine) => $call,
    }
  };
}

impl Engine for Selected {
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError> {
    dispatch!(self, engine => engine.build(spec))
  }

  fn containers(&self) -> Result<Vec<String>, EngineError> {
    dispatch!(self, engine => engine.containers())
  }

  fn delete(&self, name: &str) -> Result<(), EngineError> {
    dispatch!(self, engine => engine.delete(name))
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    dispatch!(self, engine => engine.exec(spec))
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    dispatch!(self, engine => engine.images())
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    dispatch!(self, engine => engine.run(spec))
  }

  fn running_containers(&self) -> Result<Vec<String>, EngineError> {
    dispatch!(self, engine => engine.running_containers())
  }

  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError> {
    dispatch!(self, engine => engine.start_builder(memory))
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    dispatch!(self, engine => engine.stop(name))
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    dispatch!(self, engine => engine.version())
  }
}
