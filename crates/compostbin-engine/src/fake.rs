use crate::builder::Builder;
use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{BuildPlan, ExecSpec, RunSpec};
use std::cell::RefCell;

/// A call that was made instead of run. Carries the spec rather than a rendering
/// of it, so a test asserts against what the caller composed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
  Exec(ExecSpec),
  Images,
  Run(RunSpec),
  Running,
  Stop(String),
  Version,
}

/// Records every call instead of running anything, so callers can assert what was
/// asked of the engine and that nothing else was.
#[derive(Debug, Default)]
pub struct Calls {
  recorded: RefCell<Vec<Call>>,
}

impl Calls {
  /// Every recorded call, in order.
  pub fn all(&self) -> Vec<Call> {
    self.recorded.borrow().clone()
  }

  fn record(&self, call: Call) {
    self.recorded.borrow_mut().push(call);
  }
}

/// An `Engine` that runs nothing.
#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: Calls,
  /// The containers the posed engine is running.
  running: Vec<String>,
  /// `None` poses an engine that cannot say what images exist.
  images: Option<Vec<String>>,
  version: Option<String>,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  /// Poses an engine running exactly these containers.
  pub fn with_running(containers: &[&str]) -> Self {
    Self {
      running: containers.iter().copied().map(str::to_string).collect(),
      ..Self::default()
    }
  }

  /// Poses an engine holding exactly these images.
  pub fn with_images(images: &[&str]) -> Self {
    Self {
      images: Some(images.iter().copied().map(str::to_string).collect()),
      version: Some("Containerization 0.45.0".to_string()),
      ..Self::default()
    }
  }

  pub fn calls(&self) -> Vec<Call> {
    self.calls.all()
  }
}

impl Engine for RecordingEngine {
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.calls.record(Call::Exec(spec.clone()));
    Ok(0)
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    self.calls.record(Call::Images);

    self
      .images
      .clone()
      .ok_or_else(|| EngineError::unavailable("read the image store", "there is none"))
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.calls.record(Call::Run(spec.clone()));
    Ok(spec.name.clone())
  }

  fn running_containers(&self) -> Result<Vec<String>, EngineError> {
    self.calls.record(Call::Running);

    Ok(self.running.clone())
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self.calls.record(Call::Stop(name.to_string()));
    Ok(())
  }

  fn version(&self) -> Result<Option<String>, EngineError> {
    self.calls.record(Call::Version);

    Ok(self.version.clone())
  }
}

/// A `Builder` that builds nothing and keeps the plans it was given.
///
/// The plans themselves rather than a rendering of them: a plan is what the
/// caller composed, and asserting against it reads better than asserting against
/// a string that would have to be invented for the purpose.
#[derive(Debug, Default)]
pub struct RecordingBuilder {
  plans: RefCell<Vec<BuildPlan>>,
}

impl RecordingBuilder {
  pub fn new() -> Self {
    Self::default()
  }

  /// Every plan it was asked to build, in call order.
  pub fn plans(&self) -> Vec<BuildPlan> {
    self.plans.borrow().clone()
  }
}

impl Builder for RecordingBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    self.plans.borrow_mut().push(plan.clone());
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
    engine.version().expect("version should succeed");

    assert_eq!(engine.calls(), [Call::Stop("cb-test".to_string()), Call::Version]);
  }

  #[test]
  fn records_nothing_when_unused() {
    assert!(RecordingEngine::new().calls().is_empty());
    assert!(RecordingBuilder::new().plans().is_empty());
  }

  #[test]
  fn keeps_the_plans_it_was_given() {
    let builder = RecordingBuilder::new();
    let plan = BuildPlan::new("docker.io/library/debian:stable-slim", "compostbin/base:latest");

    builder.build(&plan).expect("recording should succeed");

    assert_eq!(builder.plans(), [plan]);
  }
}
