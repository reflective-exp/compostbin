//! Turning a termination signal into the flag `host::serve` already polls.
//!
//! `host-agent` runs until something stops it, and until now the only something
//! was Ctrl-C — which kills the process outright, stranding whatever request was
//! in flight and leaving its claim in `running/` with no status file for the
//! guest to find. A signal handler that sets the flag instead lets the serve
//! loop finish what it claimed and return normally.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide because a signal handler has nowhere else to put anything: it
/// runs on whatever thread the kernel picks, with no argument of its own.
static STOP: AtomicBool = AtomicBool::new(false);

/// The signals a service is expected to shut down on: Ctrl-C from a terminal,
/// and `kill` from anything else.
const TERMINATING: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

/// Storing to an atomic is one of the few things a handler may do — it allocates
/// nothing, takes no lock, and calls nothing that could already be part-way
/// through running on the interrupted thread.
extern "C" fn stop(_signal: libc::c_int) {
  STOP.store(true, Ordering::Relaxed);
}

/// Installs the handler and hands back the flag to pass to `host::serve`.
///
/// `SA_RESETHAND` restores the default disposition as the handler runs, so the
/// *second* Ctrl-C kills the process. That is deliberate: the graceful path
/// waits for a claimed command to finish, and a `cargo test` that has hung must
/// stay interruptible.
pub fn stop_on_termination() -> Result<&'static AtomicBool, io::Error> {
  for signal in TERMINATING {
    // SAFETY: `action` is fully initialised below before it is read, and `stop`
    // is a plain `extern "C"` function with the signature `sigaction` expects.
    unsafe {
      let mut action: libc::sigaction = std::mem::zeroed();
      action.sa_sigaction = stop as *const () as libc::sighandler_t;
      action.sa_flags = libc::SA_RESETHAND;
      libc::sigemptyset(&raw mut action.sa_mask);

      if libc::sigaction(signal, &raw const action, std::ptr::null_mut()) != 0 {
        return Err(io::Error::last_os_error());
      }
    }
  }

  Ok(&STOP)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// One test, not several: the flag and the handler are process-wide, so a
  /// second test asserting the flag is *unset* would depend on running first.
  ///
  /// It raises the signal at itself, which is safe only because the handler is
  /// installed first — `SA_RESETHAND` means a second raise would terminate the
  /// process rather than set the flag again.
  #[test]
  fn a_termination_signal_asks_the_agent_to_stop() {
    let stop = stop_on_termination().expect("installing the handler should succeed");
    assert!(!stop.load(Ordering::Relaxed), "nothing has signalled yet");

    assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0, "raise should succeed");

    assert!(
      stop.load(Ordering::Relaxed),
      "the serve loop's flag is what the handler exists to set"
    );
  }
}
