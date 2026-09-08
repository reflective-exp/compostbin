use apple_container::error::EngineError;
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::io;
use std::path::{Path, PathBuf};

/// A Claude token that could not be read from the Keychain or written into
/// Claude's home.
#[derive(Debug)]
pub enum CredentialError {
  Io(PathError),
  Keychain(String),
}

impl Display for CredentialError {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
    match self {
      Self::Io(error) => error.fmt(formatter),
      Self::Keychain(message) => write!(formatter, "reading the login Keychain: {message}"),
    }
  }
}

impl Error for CredentialError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Io(error) => Some(error),
      Self::Keychain(_) => None,
    }
  }
}

/// A base image that could not be written into a build context or built.
#[derive(Debug)]
pub enum ImageError {
  Engine(EngineError),
  Io(PathError),
}

impl Display for ImageError {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
    match self {
      Self::Engine(error) => error.fmt(formatter),
      Self::Io(error) => error.fmt(formatter),
    }
  }
}

impl Error for ImageError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Engine(error) => Some(error),
      Self::Io(error) => Some(error),
    }
  }
}

impl From<EngineError> for ImageError {
  fn from(error: EngineError) -> Self {
    Self::Engine(error)
  }
}

/// A manifest that could not be read, parsed, or written. Every variant names the
/// file, since the CLI may be looking at a manifest the user did not expect.
#[derive(Debug)]
pub enum ManifestError {
  Io(PathError),
  Parse { path: PathBuf, source: toml::de::Error },
  Render { path: PathBuf, source: toml::ser::Error },
}

impl ManifestError {
  pub fn path(&self) -> &Path {
    match self {
      Self::Io(error) => error.path(),
      Self::Parse { path, .. } | Self::Render { path, .. } => path,
    }
  }
}

impl Display for ManifestError {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
    match self {
      Self::Io(error) => error.fmt(formatter),
      Self::Parse { path, source } => write!(formatter, "{}: {source}", path.display()),
      Self::Render { path, source } => write!(formatter, "{}: {source}", path.display()),
    }
  }
}

impl Error for ManifestError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Io(error) => Some(error),
      Self::Parse { source, .. } => Some(source),
      Self::Render { source, .. } => Some(source),
    }
  }
}

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
