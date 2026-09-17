use crate::builder::Builder;
use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{BuildSpec, ExecSpec, RunSpec};
use std::cell::RefCell;

/// Records the argv of every call instead of running anything, so callers can
/// assert what was invoked and that nothing else was.
#[derive(Debug, Default)]
pub struct Calls {
  recorded: RefCell<Vec<Vec<String>>>,
}

impl Calls {
  /// Every recorded argv, in call order.
  pub fn all(&self) -> Vec<Vec<String>> {
    self.recorded.borrow().clone()
  }

  fn record(&self, argv: Vec<String>) {
    self.recorded.borrow_mut().push(argv);
  }
}

/// An `Engine` that runs nothing.
#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: Calls,
  /// Every container the posed engine holds, and whether each one is running.
  containers: Vec<(String, bool)>,
  /// `None` poses an engine that cannot say what images exist.
  images: Option<Vec<String>>,
  version: Option<String>,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  /// Poses an engine holding exactly these containers, each paired with whether
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

  /// Poses an engine holding exactly these images.
  pub fn with_images(images: &[&str]) -> Self {
    Self {
      images: Some(images.iter().map(|name| name.to_string()).collect()),
      version: Some("Containerization 0.45.0".to_string()),
      ..Self::default()
    }
  }

  pub fn calls(&self) -> Vec<Vec<String>> {
    self.calls.all()
  }
}

impl Engine for RecordingEngine {
  fn containers(&self) -> Result<Vec<String>, EngineError> {
    self.calls.record(vec!["containers".to_string()]);

    Ok(
      self
        .containers
        .iter()
        .map(|(name, _)| name.clone())
        .collect(),
    )
  }

  fn delete(&self, name: &str) -> Result<(), EngineError> {
    self
      .calls
      .record(vec!["delete".to_string(), name.to_string()]);
    Ok(())
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.calls.record(spec.to_argv());
    Ok(0)
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    let argv = vec!["images".to_string()];
    self.calls.record(argv.clone());

    self.images.clone().ok_or(EngineError::Spawn {
      argv,
      source: std::io::Error::new(std::io::ErrorKind::NotFound, "no image store"),
    })
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.calls.record(spec.to_argv());
    Ok(spec.name.clone())
  }

  fn running_containers(&self) -> Result<Vec<String>, EngineError> {
    self.calls.record(vec!["running".to_string()]);

    Ok(
      self
        .containers
        .iter()
        .filter(|(_, running)| *running)
        .map(|(name, _)| name.clone())
        .collect(),
    )
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self
      .calls
      .record(vec!["stop".to_string(), name.to_string()]);
    Ok(())
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    self.calls.record(vec!["version".to_string()]);

    Ok(self.version.clone())
  }
}

/// A `Builder` that records the argv it would have run.
#[derive(Debug, Default)]
pub struct RecordingBuilder {
  calls: Calls,
}

impl RecordingBuilder {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn calls(&self) -> Vec<Vec<String>> {
    self.calls.all()
  }
}

impl Builder for RecordingBuilder {
  fn build(&self, spec: &BuildSpec) -> Result<i32, EngineError> {
    self.calls.record(spec.to_argv());
    Ok(0)
  }

  fn start_builder(&self, memory: Option<&str>) -> Result<(), EngineError> {
    let mut argv = vec!["builder".to_string(), "start".to_string()];

    if let Some(memory) = memory {
      argv.push("--memory".to_string());
      argv.push(memory.to_string());
    }

    self.calls.record(argv);
    Ok(())
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
    assert_eq!(RecordingBuilder::new().calls(), Vec::<Vec<String>>::new());
  }
}
