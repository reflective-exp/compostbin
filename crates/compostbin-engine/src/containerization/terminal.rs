//! The host end of the terminal a guest process is attached to.
//!
//! The guest has its own pty in the VM, so the host end must be raw: no line
//! buffering, echo, or signal characters. Doing it twice shows up as doubled
//! characters and a shell that only reacts on return.
//!
//! Raw mode belongs to the process that owns the terminal (the owner its own,
//! a joiner the one it passes over the control socket). The framework takes the
//! descriptor without touching attributes it doesn't own, so this is the only
//! place they change.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How often a process asks whether its window changed size.
const RESIZE_POLL: Duration = Duration::from_millis(100);

/// Runs `attached`, calling `resize` each time this process's window changes
/// size while it runs, and returns `attached`'s value.
///
/// SIGWINCH reaches whichever process holds the terminal: the owner of the VM
/// resizes the guest's pty itself, a joiner asks the owner to. Both poll a flag
/// instead of resizing in the handler, which may only touch an atomic.
pub fn while_resizing<T>(
  changed: &(dyn Fn() -> bool + Sync),
  resize: impl Fn() + Send,
  attached: impl FnOnce() -> T,
) -> T {
  let done = AtomicBool::new(false);

  std::thread::scope(|scope| {
    let done = &done;

    let watcher = scope.spawn(move || {
      while !done.load(Ordering::Relaxed) {
        if changed() {
          resize();
        }

        // Parked, not slept: the end of the attach unparks the watcher, so the
        // join below it doesn't wait out the rest of an interval.
        std::thread::park_timeout(RESIZE_POLL);
      }
    });

    // Stops the watcher on every path out of `attached`. One still looping
    // after a panic would hang `scope`'s join instead of letting it unwind.
    let _ending = Ending(|| {
      done.store(true, Ordering::Relaxed);
      watcher.thread().unpark();
    });

    attached()
  })
}

/// Runs its closure when dropped, including while a panic unwinds.
struct Ending<F: Fn()>(F);

impl<F: Fn()> Drop for Ending<F> {
  fn drop(&mut self) {
    self.0();
  }
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::AtomicUsize;

  #[test]
  fn resizes_while_the_guest_runs_and_stops_once_it_has_gone() {
    let resizes = AtomicUsize::new(0);
    let changed = || true;

    let code = while_resizing(
      &changed,
      || {
        resizes.fetch_add(1, Ordering::Relaxed);
      },
      || {
        while resizes.load(Ordering::Relaxed) == 0 {
          std::thread::sleep(Duration::from_millis(5));
        }

        7
      },
    );

    assert_eq!(code, 7, "the attach's value comes back to its caller");

    let resized = resizes.load(Ordering::Relaxed);
    std::thread::sleep(RESIZE_POLL * 3);

    assert_eq!(
      resizes.load(Ordering::Relaxed),
      resized,
      "the watcher ends with the attach"
    );
  }

  /// Without the drop guard the watcher keeps looping, and `scope`'s join hangs
  /// where it should unwind.
  #[test]
  fn ends_the_watcher_even_when_the_attach_panics() {
    let changed = || true;
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      while_resizing(&changed, || {}, || panic!("the guest went away"));
    }));

    assert!(panicked.is_err(), "the panic should reach the caller");
  }

  #[test]
  fn resizes_nothing_while_the_window_holds_its_size() {
    let resizes = AtomicUsize::new(0);
    let changed = || false;

    while_resizing(
      &changed,
      || {
        resizes.fetch_add(1, Ordering::Relaxed);
      },
      || std::thread::sleep(RESIZE_POLL * 2),
    );

    assert_eq!(resizes.load(Ordering::Relaxed), 0);
  }
}
