//! Waiting on something another process does.

use std::time::{Duration, Instant};

/// How often [`poll_until`] looks again.
const INTERVAL: Duration = Duration::from_millis(50);

/// Calls `check` until it returns something, and returns that. Fails the test
/// once `timeout` has passed, naming `what` it was waiting for.
pub fn poll_until<T>(timeout: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
  let deadline = Instant::now() + timeout;

  loop {
    if let Some(found) = check() {
      return found;
    }

    assert!(
      Instant::now() < deadline,
      "timed out after {timeout:?} waiting for {what}"
    );
    std::thread::sleep(INTERVAL);
  }
}
