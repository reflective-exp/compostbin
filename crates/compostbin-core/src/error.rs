//! A variant that only wraps another error delegates `Display` to it and returns
//! the inner error's `source`, not the inner error, so a chain never prints the
//! same message twice.

use compostbin_engine::error::EngineError;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::PathBuf;

#[derive(Debug)]
pub enum CredentialError {
  Io(PathError),
  Keychain(String),
  KeychainSave(String),
  Security(io::Error),
}

impl fmt::Display for CredentialError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io(error) => error.fmt(formatter),
      Self::Keychain(message) => write!(formatter, "reading the login Keychain: {message}"),
      Self::KeychainSave(message) => write!(formatter, "saving to the login Keychain: {message}"),
      Self::Security(error) => write!(formatter, "running `security`: {error}"),
    }
  }
}

impl Error for CredentialError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Io(error) => error.source(),
      Self::Keychain(_) | Self::KeychainSave(_) => None,
      Self::Security(error) => Some(error),
    }
  }
}

impl From<PathError> for CredentialError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

#[derive(Debug)]
pub enum ImageError {
  Engine(EngineError),
  Io(PathError),
}

impl fmt::Display for ImageError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Engine(error) => error.fmt(formatter),
      Self::Io(error) => error.fmt(formatter),
    }
  }
}

impl Error for ImageError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Engine(error) => error.source(),
      Self::Io(error) => error.source(),
    }
  }
}

impl From<EngineError> for ImageError {
  fn from(error: EngineError) -> Self {
    Self::Engine(error)
  }
}

impl From<PathError> for ImageError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

/// Also used for a session's mount record, not only the project manifest.
#[derive(Debug)]
pub enum ManifestError {
  Io(PathError),
  Parse { path: PathBuf, source: toml::de::Error },
  Render { path: PathBuf, source: toml::ser::Error },
}

impl fmt::Display for ManifestError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
      Self::Io(error) => error.source(),
      Self::Parse { source, .. } => Some(source),
      Self::Render { source, .. } => Some(source),
    }
  }
}

impl From<PathError> for ManifestError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

#[derive(Debug)]
pub enum SessionError {
  Config(ManifestError),
  Credential(CredentialError),
  Engine(EngineError),
  Io(PathError),
  Record(ManifestError),
}

impl fmt::Display for SessionError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(error) => error.fmt(formatter),
      Self::Credential(error) => error.fmt(formatter),
      Self::Engine(error) => error.fmt(formatter),
      Self::Io(error) => error.fmt(formatter),
      Self::Record(error) => write!(formatter, "recording the container's mounts: {error}"),
    }
  }
}

impl Error for SessionError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Config(error) => error.source(),
      Self::Credential(error) => error.source(),
      Self::Engine(error) => error.source(),
      Self::Io(error) => error.source(),
      Self::Record(error) => Some(error),
    }
  }
}

impl From<CredentialError> for SessionError {
  fn from(error: CredentialError) -> Self {
    Self::Credential(error)
  }
}

impl From<EngineError> for SessionError {
  fn from(error: EngineError) -> Self {
    Self::Engine(error)
  }
}

impl From<PathError> for SessionError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidMemory(pub(crate) String);

impl fmt::Display for InvalidMemory {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      formatter,
      "\"{}\" is not a memory size: expected a number, optionally followed by G, M, or K",
      self.0
    )
  }
}

impl Error for InvalidMemory {}

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

  pub fn is_not_found(&self) -> bool {
    self.source.kind() == io::ErrorKind::NotFound
  }
}

impl fmt::Display for PathError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}: {}", self.path.display(), self.source)
  }
}

impl Error for PathError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    Some(&self.source)
  }
}

/// Every `std::fs` call should go through this so its error names the path.
pub trait At<T> {
  fn at(self, path: impl Into<PathBuf>) -> Result<T, PathError>;
}

impl<T> At<T> for io::Result<T> {
  fn at(self, path: impl Into<PathBuf>) -> Result<T, PathError> {
    self.map_err(|source| PathError::new(path, source))
  }
}

/// Reported to the guest; not a failure of the agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
  ArgumentsNotAllowed(String),
  DeniedArgument(String),
  EmptyCommand(String),
  EmptyRequest,
  UnknownCommand(String),
}

impl fmt::Display for Refusal {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ArgumentsNotAllowed(name) => write!(
        formatter,
        "\"{name}\" takes no arguments; declare `arguments = true` on it to allow them"
      ),
      Self::DeniedArgument(argument) => write!(
        formatter,
        "the argument \"{argument}\" is refused by this command's `deny` list"
      ),
      Self::EmptyCommand(name) => write!(formatter, "\"{name}\" has an empty argv, so it names nothing to run"),
      Self::EmptyRequest => write!(formatter, "the request names no command"),
      Self::UnknownCommand(name) => write!(formatter, "\"{name}\" is not in [host.commands]"),
    }
  }
}

impl Error for Refusal {}
