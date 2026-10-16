use crate::error::{At, PathError};
use std::path::Path;

/// A no-op when `path` is absent; a symlink is removed, not followed.
pub(crate) fn remove(path: &Path) -> Result<(), PathError> {
  let kind = match std::fs::symlink_metadata(path).at(path) {
    Ok(metadata) => metadata.file_type(),
    Err(error) if error.is_not_found() => return Ok(()),
    Err(error) => return Err(error),
  };

  let removed = if kind.is_dir() {
    std::fs::remove_dir_all(path)
  } else {
    std::fs::remove_file(path)
  };

  removed.at(path)
}

pub(crate) fn empty(dir: &Path) -> Result<(), PathError> {
  for entry in std::fs::read_dir(dir).at(dir)? {
    remove(&entry.at(dir)?.path())?;
  }

  Ok(())
}
