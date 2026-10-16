//! The framework streams a duplicate of each descriptor, so the caller keeps its own open.

use std::os::fd::RawFd;

pub const UNATTACHED: RawFd = -1;

/// Containerization refuses a stderr beside a terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stdio {
  pub terminal: RawFd,
  pub stdin: RawFd,
  pub stdout: RawFd,
  pub stderr: RawFd,
}

impl Stdio {
  pub fn terminal(terminal: RawFd, stdout: RawFd) -> Self {
    Self {
      terminal,
      stdin: UNATTACHED,
      stdout,
      stderr: UNATTACHED,
    }
  }

  pub fn inherit(interactive: bool) -> Self {
    Self {
      terminal: UNATTACHED,
      stdin: if interactive { libc::STDIN_FILENO } else { UNATTACHED },
      stdout: libc::STDOUT_FILENO,
      stderr: libc::STDERR_FILENO,
    }
  }

  pub fn nothing() -> Self {
    Self {
      terminal: UNATTACHED,
      stdin: UNATTACHED,
      stdout: UNATTACHED,
      stderr: UNATTACHED,
    }
  }

  pub fn has_terminal(&self) -> bool {
    self.terminal != UNATTACHED
  }
}

pub fn is_tty(descriptor: RawFd) -> bool {
  // SAFETY: isatty only reads, and tolerates any integer.
  unsafe { libc::isatty(descriptor) == 1 }
}

pub fn attached(descriptor: RawFd) -> Option<RawFd> {
  (descriptor != UNATTACHED).then_some(descriptor)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn attaches_a_terminal_and_leaves_the_plain_streams_alone() {
    let stdio = Stdio::terminal(7, 9);

    assert!(stdio.has_terminal());
    assert_eq!(stdio.stdout, 9);
    assert_eq!(stdio.stdin, UNATTACHED);
    assert_eq!(stdio.stderr, UNATTACHED);
  }

  #[test]
  fn leaves_out_stdin_for_a_process_that_reads_none() {
    assert_eq!(Stdio::inherit(false).stdin, UNATTACHED);
    assert_eq!(Stdio::inherit(true).stdin, libc::STDIN_FILENO);
  }

  #[test]
  fn hands_the_framework_only_the_streams_the_caller_attached() {
    assert_eq!(attached(UNATTACHED), None);
    assert_eq!(attached(7), Some(7));
  }
}
