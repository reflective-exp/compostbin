use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{ExecSpec, RunSpec};
use std::cell::RefCell;

/// An `Engine` that records the argv of every call instead of running anything.
/// Lets callers assert both what was invoked and — just as importantly — that
/// nothing was invoked.
#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: RefCell<Vec<Vec<String>>>,
  exec_exit_code: i32,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_exec_exit_code(exec_exit_code: i32) -> Self {
    Self {
      calls: RefCell::new(Vec::new()),
      exec_exit_code,
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
  fn delete(&self, name: &str) -> Result<(), EngineError> {
    self.record(vec!["delete".to_string(), name.to_string()]);
    Ok(())
  }

  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.record(spec.to_argv());
    Ok(self.exec_exit_code)
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    self.record(spec.to_argv());
    Ok(spec.name.clone())
  }

  fn stop(&self, name: &str) -> Result<(), EngineError> {
    self.record(vec!["stop".to_string(), name.to_string()]);
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
  }
}
