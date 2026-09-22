//! What this crate could not do: the attempt, and what it said.

use std::fmt;

#[derive(Debug)]
pub enum Error {
  /// Attempted and failed. The message is whatever Containerization said,
  /// which is the only account of a failure inside the VM.
  Failed { action: String, message: String },
  /// Couldn't be attempted: a prerequisite is missing or unreadable. The fix
  /// is to provision it, not to debug a failure.
  Unavailable { action: String, message: String },
}

impl Error {
  pub fn failed(action: impl Into<String>, message: impl fmt::Display) -> Self {
    Self::Failed {
      action: action.into(),
      message: message.to_string(),
    }
  }

  pub fn unavailable(action: impl Into<String>, message: impl fmt::Display) -> Self {
    Self::Unavailable {
      action: action.into(),
      message: message.to_string(),
    }
  }

  /// What was attempted, for a caller rewording the failure.
  pub fn action(&self) -> &str {
    match self {
      Self::Failed { action, .. } | Self::Unavailable { action, .. } => action,
    }
  }
}

impl fmt::Display for Error {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Failed { action, message } => write!(formatter, "could not {action}: {message}"),
      Self::Unavailable { action, message } => write!(formatter, "cannot {action}: {message}"),
    }
  }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn says_what_was_attempted_and_what_it_said() {
    assert_eq!(
      Error::failed("boot session-one", "no kernel at /nowhere").to_string(),
      "could not boot session-one: no kernel at /nowhere"
    );
    assert_eq!(
      Error::unavailable("read the image store", "it has never been provisioned").to_string(),
      "cannot read the image store: it has never been provisioned"
    );
  }

  #[test]
  fn names_the_action_without_its_message() {
    assert_eq!(Error::failed("boot session-one", "nope").action(), "boot session-one");
  }
}
