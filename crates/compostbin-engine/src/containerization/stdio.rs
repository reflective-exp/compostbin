//! The descriptors a guest process runs against.
//!
//! Each is the caller's own, so whatever its shell redirected stays redirected:
//! the guest's output is written where the caller's descriptor points, not
//! relayed through this process. The framework streams a duplicate of each, so
//! the caller keeps its own open.

use std::os::fd::RawFd;

/// A stream the caller leaves unattached; the guest neither reads nor writes it.
pub const UNATTACHED: RawFd = -1;

/// A guest process's streams: its terminal, which it reads and sizes itself
/// against, and whichever of stdin, stdout and stderr the caller attached.
///
/// A process on a terminal has no separate stderr. One pty carries every stream
/// it writes, so by the time the bytes leave the guest nothing distinguishes
/// them, and Containerization refuses a stderr beside a terminal.
///
/// Copied freely: this is four descriptor numbers and owns none of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stdio {
  pub terminal: RawFd,
  pub stdin: RawFd,
  pub stdout: RawFd,
  pub stderr: RawFd,
}

impl Stdio {
  /// A terminal the guest reads, writing what it has to say to `stdout`.
  ///
  /// Its stderr arrives there too; see the note on the type.
  pub fn terminal(terminal: RawFd, stdout: RawFd) -> Self {
    Self {
      terminal,
      stdin: UNATTACHED,
      stdout,
      stderr: UNATTACHED,
    }
  }

  /// This process's own streams, leaving out stdin when the guest process is
  /// to read none.
  pub fn inherit(interactive: bool) -> Self {
    Self {
      terminal: UNATTACHED,
      stdin: if interactive { libc::STDIN_FILENO } else { UNATTACHED },
      stdout: libc::STDOUT_FILENO,
      stderr: libc::STDERR_FILENO,
    }
  }

  /// Nothing attached at all: a process that neither reads nor writes.
  pub fn nothing() -> Self {
    Self {
      terminal: UNATTACHED,
      stdin: UNATTACHED,
      stdout: UNATTACHED,
      stderr: UNATTACHED,
    }
  }

  /// Whether the process reads a terminal, and so can be resized.
  pub fn has_terminal(&self) -> bool {
    self.terminal != UNATTACHED
  }
}

/// Whether a descriptor is a terminal. When stdin or stdout isn't (e.g.
/// `run > log`), a caller can run the guest on plain streams rather than fail.
pub fn is_tty(descriptor: RawFd) -> bool {
  // SAFETY: isatty only reads, and tolerates any integer.
  unsafe { libc::isatty(descriptor) == 1 }
}

/// `descriptor` as the framework takes it.
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
