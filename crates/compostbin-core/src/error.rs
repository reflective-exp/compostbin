//! Every domain's failures, since all of them reach the same CLI.
//!
//! A variant that only widens a type (`SessionError::Io`, say) delegates
//! `Display` to what it holds, so it forwards the inner error's `source` rather
//! than naming the inner error, which would print the same sentence twice down
//! a chain. A variant with a message of its own names what it holds as the
//! source.

use compostbin_engine::error::EngineError;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A Claude token that could not be read from the Keychain or written into
/// Claude's home.
#[derive(Debug)]
pub enum CredentialError {
  Io(PathError),
  Keychain(String),
}

impl fmt::Display for CredentialError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io(error) => error.fmt(formatter),
      Self::Keychain(message) => write!(formatter, "reading the login Keychain: {message}"),
    }
  }
}

impl Error for CredentialError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Io(error) => error.source(),
      Self::Keychain(_) => None,
    }
  }
}

impl From<PathError> for CredentialError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

/// A base image that could not be written into a build context or built.
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

/// A TOML file compostbin owns — the project manifest, or a session's mount
/// record — that could not be read, parsed, or written. Every variant names the
/// file, since it may not be the manifest the user expected.
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

/// A session that could not be prepared or created, or whose mount record could
/// not be written. One error because `run` does all three as one step.
#[derive(Debug)]
pub enum SessionError {
  Credential(CredentialError),
  Engine(EngineError),
  Io(PathError),
  Record(ManifestError),
}

impl fmt::Display for SessionError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
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
      Self::Credential(error) => error.source(),
      Self::Engine(error) => error.source(),
      Self::Io(error) => error.source(),
      // Has its own message, so the inner error is the source.
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

impl From<ManifestError> for SessionError {
  fn from(error: ManifestError) -> Self {
    Self::Record(error)
  }
}

/// A filesystem error that names its path, which `std::io::Error` alone does
/// not.
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

  /// Whether nothing was there. Several callers treat that as ordinary (no
  /// local manifest, no record, no spool yet) rather than a failure.
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

/// Attaches the failing path to an `io::Result`: `.at(&path)?`. Every `std::fs`
/// call in compostbin goes through this, and every error type that holds a
/// `PathError` converts from it, so `?` does the rest.
pub trait At<T> {
  fn at(self, path: impl Into<PathBuf>) -> Result<T, PathError>;
}

impl<T> At<T> for io::Result<T> {
  fn at(self, path: impl Into<PathBuf>) -> Result<T, PathError> {
    self.map_err(|source| PathError::new(path, source))
  }
}

/// Why a host command was refused — an outcome reported to the guest, not a
/// failure of the agent. Separate from `HostError` so it compares by value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
  /// Declared without `arguments = true`.
  ArgumentsNotAllowed(String),
  DeniedArgument(String),
  /// Declared with an empty `argv`.
  EmptyCommand(String),
  EmptyRequest,
  NewlineInArgument(String),
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
      Self::NewlineInArgument(argument) => write!(
        formatter,
        "an argument may not contain a newline, which \"{argument}\" does"
      ),
      Self::UnknownCommand(name) => write!(formatter, "\"{name}\" is not in [host.commands]"),
    }
  }
}

impl Error for Refusal {}

/// A host command that could not be submitted or served.
#[derive(Debug)]
pub enum HostError {
  Io(PathError),
  Refused(Refusal),
}

impl fmt::Display for HostError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io(error) => error.fmt(formatter),
      Self::Refused(refusal) => refusal.fmt(formatter),
    }
  }
}

impl Error for HostError {
  fn source(&self) -> Option<&(dyn Error + 'static)> {
    match self {
      Self::Io(error) => error.source(),
      Self::Refused(refusal) => refusal.source(),
    }
  }
}

impl From<PathError> for HostError {
  fn from(error: PathError) -> Self {
    Self::Io(error)
  }
}

impl From<Refusal> for HostError {
  fn from(refusal: Refusal) -> Self {
    Self::Refused(refusal)
  }
}
