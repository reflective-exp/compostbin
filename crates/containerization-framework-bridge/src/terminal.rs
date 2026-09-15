//! The host end of an attached terminal.
//!
//! The guest process gets a pty of its own, inside the VM. For the two to
//! behave like one terminal, the host end has to stop interpreting: no line
//! buffering, no echo, no signal characters — the guest's pty is doing all of
//! that, and doing it twice shows up as doubled characters and a shell that
//! only reacts when you press return.
//!
//! Raw mode belongs to whichever process owns the terminal, which is why it is
//! set here rather than in Swift: `run` owns its own, and `shell` owns the one
//! it passes over the control socket. Swift takes the descriptor with
//! `setInitState: false` precisely so it never touches attributes that are not
//! its to restore.

use std::io;
use std::os::fd::RawFd;

/// Whether a descriptor is a terminal at all.
///
/// Output redirected to a file is the ordinary case — `compostbin run > log` —
/// and the guest process should then simply run without a terminal rather than
/// failing on one that is not there.
pub fn is_tty(descriptor: RawFd) -> bool {
  // SAFETY: isatty only reads, and tolerates any integer.
  unsafe { libc::isatty(descriptor) == 1 }
}

/// Duplicates a descriptor for the Swift side to own.
///
/// Swift closes it when the attach ends, and this side must not: the close is
/// what stops reading, and it has to happen exactly once.
///
/// `LinuxProcess` pumps stdin with a task reading the terminal it is given, and
/// tearing a process down only cancels that task — which does not interrupt a
/// read already pending on the handle. Until the descriptor is closed, that
/// reader is still on the terminal, taking keystrokes that now belong to
/// whoever is typing at it. A duplicate is what makes closing safe: the caller
/// keeps its own descriptor for the same terminal, so the terminal itself
/// survives.
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
/// Restoring on drop rather than at the end of the attach matters: the guest
/// exiting, an error on the way, and a panic all have to leave the user with a
/// usable shell.
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
