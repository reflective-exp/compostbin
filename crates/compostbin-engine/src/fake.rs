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
  IsRunning(String),
  IsUnpacked(String),
  Run(RunSpec),
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
  /// The containers the posed engine is running, and every one `run` has
  /// started since.
  running: RefCell<Vec<String>>,
  /// The images the posed engine has unpacked, and every one `run` has
  /// unpacked since.
  unpacked: RefCell<Vec<String>>,
  /// `None` poses an engine that cannot say what images exist.
  images: Option<Vec<String>>,
  version: Option<String>,
  /// What every `exec` exits with.
  exit_code: i32,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  /// Poses an engine running exactly these containers.
  pub fn with_running(containers: &[&str]) -> Self {
    Self {
      running: RefCell::new(containers.iter().copied().map(str::to_string).collect()),
      ..Self::default()
    }
  }

  /// Poses an engine that has already unpacked these images.
  pub fn with_unpacked(images: &[&str]) -> Self {
    Self {
      unpacked: RefCell::new(images.iter().copied().map(str::to_string).collect()),
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

  /// Poses an engine whose every `exec` exits with this code.
  pub fn exiting_with(code: i32) -> Self {
    Self {
      exit_code: code,
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
    Ok(self.exit_code)
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
    self.running.borrow_mut().push(spec.name.clone());
    self.unpacked.borrow_mut().push(spec.image.clone());
    Ok(spec.name.clone())
  }

  fn is_running(&self, name: &str) -> Result<bool, EngineError> {
    self.calls.record(Call::IsRunning(name.to_string()));

    Ok(self.running.borrow().iter().any(|running| running == name))
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    self.calls.record(Call::IsUnpacked(image.to_string()));

    Ok(
      self
        .unpacked
        .borrow()
        .iter()
        .any(|unpacked| unpacked == image),
    )
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
  use crate::model::Resources;

  #[test]
  fn records_calls_in_order() {
    let engine = RecordingEngine::new();

    engine
      .is_running("cb-test")
      .expect("is_running should succeed");
    engine.version().expect("version should succeed");

    assert_eq!(engine.calls(), [Call::IsRunning("cb-test".to_string()), Call::Version]);
  }

  #[test]
  fn records_nothing_when_unused() {
    assert!(RecordingEngine::new().calls().is_empty());
    assert!(RecordingBuilder::new().plans().is_empty());
  }

  #[test]
  fn keeps_the_plans_it_was_given() {
    let builder = RecordingBuilder::new();
    let plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    builder.build(&plan).expect("recording should succeed");

    assert_eq!(builder.plans(), [plan]);
  }
}
