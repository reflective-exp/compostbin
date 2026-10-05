//! Fakes for the traits here, so code generic over them can be tested without
//! an engine.

use crate::builder::Builder;
use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{BuildPlan, ExecSpec, RunSpec};
use std::cell::RefCell;

/// What a posed engine reports as its version.
pub const VERSION: &str = "fake engine 1.0";

/// A recorded call, carrying the spec itself so tests assert on what the
/// caller composed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
  Exec(ExecSpec),
  Images,
  IsRunning(String),
  IsUnpacked(String),
  Run(RunSpec),
  Version,
}

/// The calls a fake has been given, in order.
#[derive(Debug)]
struct Calls<C> {
  recorded: RefCell<Vec<C>>,
}

impl<C> Default for Calls<C> {
  fn default() -> Self {
    Self {
      recorded: RefCell::default(),
    }
  }
}

impl<C: Clone> Calls<C> {
  /// Every recorded call, in order.
  fn all(&self) -> Vec<C> {
    self.recorded.borrow().clone()
  }

  fn record(&self, call: C) {
    self.recorded.borrow_mut().push(call);
  }
}

/// An `Engine` that runs nothing and records what it was asked.
#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: Calls<Call>,
  /// The containers posed as running, plus every one `run` has started.
  running: RefCell<Vec<String>>,
  /// The images posed as unpacked, plus every one `run` has unpacked.
  unpacked: RefCell<Vec<String>>,
  /// `None` poses an engine that cannot say what images exist.
  images: Option<Vec<String>>,
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

  /// Poses a working engine holding exactly these images.
  pub fn with_images(images: &[&str]) -> Self {
    Self {
      images: Some(images.iter().copied().map(str::to_string).collect()),
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

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError> {
    self.calls.record(Call::Run(spec.clone()));
    self.running.borrow_mut().push(spec.name.clone());
    self.unpacked.borrow_mut().push(spec.image.clone());
    Ok(())
  }

  fn is_running(&self, name: &str) -> bool {
    self.calls.record(Call::IsRunning(name.to_string()));

    self.running.borrow().iter().any(|running| running == name)
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

  fn version(&self) -> String {
    self.calls.record(Call::Version);

    VERSION.to_string()
  }
}

/// What a posed builder has been asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildCall {
  Build(BuildPlan),
  Provision,
}

/// A `Builder` that builds nothing and keeps the plans it was given.
#[derive(Debug, Default)]
pub struct RecordingBuilder {
  calls: Calls<BuildCall>,
}

impl RecordingBuilder {
  pub fn new() -> Self {
    Self::default()
  }

  /// Everything asked of it, in order.
  pub fn calls(&self) -> Vec<BuildCall> {
    self.calls.all()
  }

  /// Every plan given, in call order.
  pub fn plans(&self) -> Vec<BuildPlan> {
    self
      .calls
      .all()
      .into_iter()
      .filter_map(|call| match call {
        BuildCall::Build(plan) => Some(plan),
        BuildCall::Provision => None,
      })
      .collect()
  }
}

impl Builder for RecordingBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    self.calls.record(BuildCall::Build(plan.clone()));
    Ok(())
  }

  fn provision(&self) -> Result<(), EngineError> {
    self.calls.record(BuildCall::Provision);
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

    engine.is_running("cb-test");
    engine.version();

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
