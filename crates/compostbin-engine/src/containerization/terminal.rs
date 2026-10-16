//! The host end must be raw because the guest has its own pty.

use containerization_framework::containerization_os::terminal::Size;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const RESIZE_POLL: Duration = Duration::from_millis(100);

/// Process-wide; a process attaches at most one terminal.
static RESIZED: AtomicBool = AtomicBool::new(false);

pub fn watch_for_resize() {
  static INSTALLED: std::sync::Once = std::sync::Once::new();

  INSTALLED.call_once(|| {
    // SAFETY: `note_resize` only stores to an atomic, which is async-signal-safe.
    unsafe {
      libc::signal(libc::SIGWINCH, note_resize as *const () as libc::sighandler_t);
    }
  });
}

extern "C" fn note_resize(_signal: libc::c_int) {
  RESIZED.store(true, Ordering::Relaxed);
}

pub fn resized() -> bool {
  RESIZED.swap(false, Ordering::Relaxed)
}

/// Polls `changed` because the SIGWINCH handler may only touch an atomic.
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

        // Parked, not slept, so the end of the attach can wake it.
        std::thread::park_timeout(RESIZE_POLL);
      }
    });

    // Stops the watcher on every path out, including a panic, so `scope`'s join cannot hang.
    let _ending = Ending(|| {
      done.store(true, Ordering::Relaxed);
      watcher.thread().unpark();
    });

    attached()
  })
}

struct Ending<F: Fn()>(F);

impl<F: Fn()> Drop for Ending<F> {
  fn drop(&mut self) {
    self.0();
  }
}

pub fn size(descriptor: RawFd) -> io::Result<Size> {
  // SAFETY: TIOCGWINSZ fills the struct or returns < 0 and leaves it alone.
  let window = unsafe {
    let mut window: libc::winsize = std::mem::zeroed();

    if libc::ioctl(descriptor, libc::TIOCGWINSZ, &mut window) < 0 {
      return Err(io::Error::last_os_error());
    }

    window
  };

  Ok(Size {
    width: window.ws_col,
    height: window.ws_row,
  })
}

/// Raw mode until dropped, including on panic.
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
