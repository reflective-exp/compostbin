use crate::error::EngineError;
use crate::model::Resources;

pub trait Images {
  /// Each image as `name:tag`.
  fn references(&self) -> Result<Vec<String>, EngineError>;

  /// Whether booting `image` with `resources` can skip per-image preparation, such as unpacking a root filesystem.
  fn is_prepared(&self, image: &str, resources: Resources) -> Result<bool, EngineError>;

  /// The first piece of `image` missing from the store, described for a person, such as `blob sha256:<hex>`.
  fn missing(&self, image: &str) -> Result<Option<String>, EngineError>;
}
