//! Building images.
//!
//! Separate from `Engine` because a builder is not a session and a session never
//! builds: the two happen to be the same code today, and there is no reason they
//! have to be.
//!
//! One method, and nothing to start first: a builder runs the plan's steps
//! itself.

use crate::error::EngineError;
use crate::model::BuildPlan;

pub trait Builder {
  /// Builds the plan's image, streaming the build log as it happens. Returns
  /// when the image is in the store under `plan.tag`.
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError>;
}
