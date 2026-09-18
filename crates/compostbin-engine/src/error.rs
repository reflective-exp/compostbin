use std::error::Error;
use std::fmt;

/// Something the engine or the builder could not do.
///
/// Named by the action rather than by a command line: nothing here shells out any
/// more, so an argv would be a fiction. What a message has to carry instead is
/// what was attempted and what the attempt said.
#[derive(Debug)]
pub enum EngineError {
  /// It was attempted and it failed.
  Failed { action: String, message: String },
  /// It could not be attempted: something the engine needs is missing or
  /// unreadable. Distinct from `Failed` because the fix is different — provision
  /// the thing, rather than look at why it failed.
  Unavailable { action: String, message: String },
}

impl EngineError {
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

  pub fn action(&self) -> &str {
    match self {
      Self::Failed { action, .. } | Self::Unavailable { action, .. } => action,
    }
  }
}

impl fmt::Display for EngineError {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Failed { action, message } => write!(formatter, "could not {action}: {message}"),
      Self::Unavailable { action, message } => write!(formatter, "cannot {action}: {message}"),
    }
  }
}

impl Error for EngineError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn says_what_was_attempted_and_what_it_said() {
    assert_eq!(
      EngineError::failed("boot compostbin-cb", "no kernel at /nowhere").to_string(),
      "could not boot compostbin-cb: no kernel at /nowhere"
    );
    assert_eq!(
      EngineError::unavailable("read the image store", "it has never been built").to_string(),
      "cannot read the image store: it has never been built"
    );
  }
}
