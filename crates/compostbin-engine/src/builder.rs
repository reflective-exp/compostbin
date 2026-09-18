//! Building images. Separate from `Engine`: a session never builds, and a
//! build is not a session.

use crate::error::EngineError;
use crate::model::BuildPlan;

pub trait Builder {
  /// Builds the plan's image, streaming the log. Returns once it is stored
  /// under `plan.tag`.
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError>;
}
