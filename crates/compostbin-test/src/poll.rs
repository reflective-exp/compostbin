use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_millis(50);

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
