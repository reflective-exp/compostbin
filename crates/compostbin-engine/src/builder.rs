use crate::error::EngineError;
use crate::model::BuildPlan;

pub trait Builder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError>;

  /// Idempotent.
  fn provision(&self) -> Result<(), EngineError>;
}
