//! The host end of an attached terminal.
//!
//! The guest has its own pty in the VM, so the host end must be raw: no line
//! buffering, echo, or signal characters. Doing it twice shows up as doubled
//! characters and a shell that only reacts on return.
//!
//! Raw mode belongs to the process that owns the terminal (the owner its own,
//! a joiner the one it passes over the control socket). Swift takes the
//! descriptor with `setInitState: false` so it never touches attributes it
//! doesn't own.

use std::io;
use std::os::fd::RawFd;

/// Whether a descriptor is a terminal. When stdin or stdout isn't (e.g.
/// `run > log`), the guest runs on plain streams rather than failing.
pub fn is_tty(descriptor: RawFd) -> bool {
  // SAFETY: isatty only reads, and tolerates any integer.
  unsafe { libc::isatty(descriptor) == 1 }
}

/// Duplicates a descriptor for the Swift side to own.
///
/// Swift closes it when the attach ends, and this side must not: the close is
/// what stops reading, and must happen exactly once.
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

/// Puts a terminal in raw mode for as long as it is held.
///
/// Restores on drop so guest exit, errors, and panics all leave a usable shell.
pub struct Raw {
  descriptor: RawFd,
  restore: libc::termios,
}

impl Raw {
  pub fn acquire(descriptor: RawFd) -> io::Result<Self> {
    // SAFETY: tcgetattr fills the struct or returns < 0 and leaves it alone.
    let restore = unsafe {
      let mut attributes: libc::termios = std::mem::zeroed();

      if libc::tcgetattr(descriptor, &mut attributes) < 0 {
        return Err(io::Error::last_os_error());
      }

      attributes
    };

    // SAFETY: `raw` is a copy of attributes we just read from this descriptor.
    unsafe {
      let mut raw = restore;
      libc::cfmakeraw(&mut raw);

      if libc::tcsetattr(descriptor, libc::TCSANOW, &raw) < 0 {
        return Err(io::Error::last_os_error());
      }
    }

    Ok(Self { descriptor, restore })
  }
}

impl Drop for Raw {
  fn drop(&mut self) {
    // Nothing useful to do if this fails, and it runs on the panic path.
    // SAFETY: restoring attributes this type read from the same descriptor.
    unsafe {
      libc::tcsetattr(self.descriptor, libc::TCSANOW, &self.restore);
    }
  }
}
