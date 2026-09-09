use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{BuildSpec, ExecSpec, RunSpec};
use std::cell::RefCell;

/// An `Engine` that records the argv of every call instead of running anything,
/// so callers can assert what was invoked and that nothing else was.
#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: RefCell<Vec<Vec<String>>>,
  /// Every container the posed daemon holds, and whether each one is running.
  containers: Vec<(String, bool)>,
  exec_exit_code: i32,
  /// `None` poses a daemon that has not been started, where every plugin call
  /// fails.
  images: Option<Vec<String>>,
  version: Option<String>,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_exec_exit_code(exec_exit_code: i32) -> Self {
    Self {
      exec_exit_code,
      ..Self::default()
    }
  }

  /// Poses a daemon holding exactly these containers, each paired with whether
  /// it is running.
  pub fn with_containers(containers: &[(&str, bool)]) -> Self {
    Self {
      containers: containers
        .iter()
        .map(|(name, running)| (name.to_string(), *running))
        .collect(),
      ..Self::default()
    }
  }

  /// Poses a running daemon holding exactly these images.
  pub fn with_images(images: &[&str]) -> Self {
    Self {
      images: Some(images.iter().map(|name| name.to_string()).collect()),
      version: Some("1.3.1".to_string()),
      ..Self::default()
    }
  }

  /// Every recorded argv, in call order.
  pub fn calls(&self) -> Vec<Vec<String>> {
    self.calls.borrow().clone()
  }

  fn record(&self, argv: Vec<String>) {
    self.calls.borrow_mut().push(argv);
  }
}

impl Engine for RecordingEngine {
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError> {
    self.record(spec.to_argv());
    Ok(0)
  }

  fn containers(&self) -> Result<Vec<String>, EngineError> {
    self.record(vec!["ls".to_string(), "--all".to_string(), "--quiet".to_string()]);

    Ok(
      self
        .containers
        .iter()
        .map(|(name, _)| name.clone())
        .collect(),
    )
  }

  fn delete(&self, name: &str) -> Result<(), EngineError> {
    self.record(vec!["delete".to_string(), name.to_string()]);
    Ok(())
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.record(spec.to_argv());
    Ok(self.exec_exit_code)
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    let argv = vec!["image".to_string(), "ls".to_string(), "--quiet".to_string()];
    self.record(argv.clone());

    self.images.clone().ok_or(EngineError::Spawn {
      argv,
      source: std::io::Error::new(std::io::ErrorKind::NotFound, "Error: Plugins are unavailable."),
    })
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.record(spec.to_argv());
    Ok(spec.name.clone())
  }

  fn running_containers(&self) -> Result<Vec<String>, EngineError> {
    self.record(vec!["ls".to_string(), "--quiet".to_string()]);

    Ok(
      self
        .containers
        .iter()
        .filter(|(_, running)| *running)
        .map(|(name, _)| name.clone())
        .collect(),
    )
  }

  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError> {
    let mut argv = vec!["builder".to_string(), "start".to_string()];

    if let Some(memory) = memory {
      argv.push("--memory".to_string());
      argv.push(memory.to_string());
    }

    self.record(argv);
    Ok(())
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self.record(vec!["stop".to_string(), name.to_string()]);
    Ok(())
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    self.record(vec!["--version".to_string()]);

    Ok(self.version.clone())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn records_calls_in_order() {
    let engine = RecordingEngine::new();

    engine.stop("cb-test").expect("stop should succeed");
    engine.delete("cb-test").expect("delete should succeed");

    assert_eq!(engine.calls(), [vec!["stop", "cb-test"], vec!["delete", "cb-test"]]);
  }

  #[test]
  fn records_nothing_when_unused() {
    assert_eq!(RecordingEngine::new().calls(), Vec::<Vec<String>>::new());
  }
}
