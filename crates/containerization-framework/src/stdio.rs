//! The descriptors a guest process runs against.
//!
//! Each is the caller's own, so whatever its shell redirected stays redirected:
//! the guest's output is written where the caller's descriptor points, not
//! relayed through this process.

use std::io;
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
/// Copied freely: this is four descriptor numbers and owns none of them. Which
/// copy is closed, and when, is [`Stdio::try_clone`]'s business.
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

  /// The same streams, on descriptors of their own.
  ///
  /// [`crate::Session::exec`] closes what it is handed, and a caller keeps its
  /// own streams open, so each side works from a duplicate.
  pub fn try_clone(&self) -> io::Result<Self> {
    Ok(Self {
      terminal: lend_attached(self.terminal)?,
      stdin: lend_attached(self.stdin)?,
      stdout: lend_attached(self.stdout)?,
      stderr: lend_attached(self.stderr)?,
    })
  }

  /// Whether the process reads a terminal, and so can be resized.
  pub fn has_terminal(&self) -> bool {
    self.terminal != UNATTACHED
  }
}

fn lend_attached(descriptor: RawFd) -> io::Result<RawFd> {
  if descriptor == UNATTACHED {
    return Ok(UNATTACHED);
  }

  lend(descriptor)
}

/// Duplicates a descriptor for the Swift side to own.
///
/// It closes the duplicate when the attach ends, and this side must not: the
/// close is what stops reading, and must happen exactly once.
///
/// `LinuxProcess` pumps stdin with a task reading the descriptor; cancelling it
/// doesn't interrupt a pending read, so until the descriptor closes the stale
/// reader keeps stealing input. Duplicating makes that close safe, since the
/// caller's own descriptor keeps its stream open.
pub fn lend(descriptor: RawFd) -> io::Result<RawFd> {
  // SAFETY: dup only reads the descriptor table, and returns < 0 on failure.
  let lent = unsafe { libc::dup(descriptor) };

  if lent < 0 {
    return Err(io::Error::last_os_error());
  }

  Ok(lent)
}

/// Whether a descriptor is a terminal. When stdin or stdout isn't (e.g.
/// `run > log`), a caller can run the guest on plain streams rather than fail.
pub fn is_tty(descriptor: RawFd) -> bool {
  // SAFETY: isatty only reads, and tolerates any integer.
  unsafe { libc::isatty(descriptor) == 1 }
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
  fn duplicates_only_the_streams_the_caller_attached() {
    let cloned = Stdio::inherit(false)
      .try_clone()
      .expect("the process's own streams should duplicate");

    assert_eq!(cloned.terminal, UNATTACHED);
    assert_eq!(cloned.stdin, UNATTACHED);
    assert_ne!(cloned.stdout, libc::STDOUT_FILENO, "stdout should be a duplicate");

    for descriptor in [cloned.stdout, cloned.stderr] {
      // SAFETY: closing descriptors this test just made.
      unsafe { libc::close(descriptor) };
    }
  }

  #[test]
  fn nothing_is_attached_to_nothing() {
    assert_eq!(
      Stdio::nothing(),
      Stdio {
        terminal: UNATTACHED,
        stdin: UNATTACHED,
        stdout: UNATTACHED,
        stderr: UNATTACHED,
      }
    );
  }
}
