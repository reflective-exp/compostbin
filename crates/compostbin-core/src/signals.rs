//! Turning a termination signal into the flag `host::serve` polls.
//!
//! An unhandled Ctrl-C kills `host-agent` outright, stranding the request in
//! flight and leaving its claim in `running/` with no status file for the guest
//! to find. Setting the flag instead lets the serve loop finish and return.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide because a handler runs on whatever thread the kernel picks, with
/// no argument of its own.
static STOP: AtomicBool = AtomicBool::new(false);

/// The signals a service is expected to shut down on: Ctrl-C from a terminal,
/// and `kill` from anything else.
const TERMINATING: [libc::c_int; 2] = [libc::SIGINT, libc::SIGTERM];

/// Storing to an atomic is async-signal-safe: it allocates nothing, takes no
/// lock, and calls nothing the interrupted thread could be part-way through.
extern "C" fn stop(_signal: libc::c_int) {
  STOP.store(true, Ordering::Relaxed);
}

/// Installs the handler and hands back the flag to pass to `host::serve`.
///
/// `SA_RESETHAND` restores the default disposition as the handler runs, so the
/// *second* Ctrl-C kills the process: the graceful path waits for a claimed
/// command to finish, and a hung `cargo test` must stay interruptible.
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
  /// It raises the signal at itself, which is safe only because the handler is
  /// installed first — under `SA_RESETHAND` a second raise would terminate the
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
