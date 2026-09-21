//! Building images. Separate from `Engine` because a session never builds.

use crate::error::EngineError;
use crate::model::BuildPlan;

pub trait Builder {
  /// Builds the plan's image, streaming the log. Returns once it is stored
  /// under `plan.tag`.
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError>;

  /// Fetches what a build needs beyond the plan. Idempotent and cheap.
  fn provision(&self) -> Result<(), EngineError>;
}
