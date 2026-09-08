use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::io;
use std::path::{Path, PathBuf};

/// A filesystem error that remembers which path caused it. `std::io::Error` alone
/// reports "No such file or directory" without naming the file, which is useless
/// in `add` and `doctor` output.
#[derive(Debug)]
pub struct PathError {
  path: PathBuf,
  source: io::Error,
}

impl PathError {
  pub fn new(path: impl Into<PathBuf>, source: io::Error) -> Self {
    Self {
      path: path.into(),
      source,
    }
  }

  pub fn path(&self) -> &Path {
    &self.path
  }
}

impl Display for PathError {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
    write!(formatter, "{}: {}", self.path.display(), self.source)
  }
}

impl Error for PathError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    Some(&self.source)
  }
}
