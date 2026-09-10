#![cfg(feature = "integration")]
//! Measures guest-to-host streaming over a bind mount: the guest appends to a
//! file while the host reads it.
//!
//! The other direction does not work. A file the guest opens while the host is
//! still appending to it stays stale in the guest permanently — it never sees a
//! byte written after it first looked — which is why command output travels as
//! numbered chunks, each renamed into place so every file the guest opens is
//! already complete.
//!
//! Guest input takes the opposite path: the guest appends, and `pump_stdin`
//! reads as it arrives. That is only sound if the asymmetry is real, so this
//! measures it rather than assuming it.
//!
//! Run it from inside a session, via `bin/test/guest-input-streaming`, which
//! starts the guest half and then asks the host to run this test. Run alone it
//! fails waiting for a guest that is not there, which is the honest outcome —
//! half a measurement is worse than none.
//!
//! The two halves rendezvous through `target/guest-input-streaming`, which each
//! reaches over the repo mount at its own path. The host publishes `ready` and
//! only then begins looking, so its first look at `stream` lands while the guest
//! is still writing. A look that arrives afterwards measures whole-file
//! propagation, which already works and is not the question.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Finer than the guest's write interval, so the host can catch the file partly
/// written rather than only in the gaps between whole ones.
const POLL: Duration = Duration::from_millis(25);
/// The guest half is started by hand, so this is patience for a person.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// A hung guest must fail rather than hang the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// One non-empty read: when it happened, and how much it returned.
struct Batch {
  at: Duration,
  bytes: usize,
}

/// What one reading strategy saw over the run.
struct Observation {
  label: &'static str,
  batches: Vec<Batch>,
}

impl Observation {
  fn new(label: &'static str) -> Self {
    Self {
      label,
      batches: Vec::new(),
    }
  }

  fn record(&mut self, at: Duration, bytes: usize) {
    if bytes > 0 {
      self.batches.push(Batch { at, bytes });
    }
  }

  fn total(&self) -> usize {
    self.batches.iter().map(|batch| batch.bytes).sum()
  }

  fn report(&self) {
    let first = self.batches.first().map(|batch| batch.at);
    let last = self.batches.last().map(|batch| batch.at);
    println!(
      "  {:<7} {:>7} bytes in {:>3} reads, first at {:>7}, last at {:>7}",
      self.label,
      self.total(),
      self.batches.len(),
      first.map_or("never".to_string(), format_elapsed),
      last.map_or("never".to_string(), format_elapsed),
    );
  }
}

fn format_elapsed(elapsed: Duration) -> String {
  format!("{}ms", elapsed.as_millis())
}

fn shared_directory() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/guest-input-streaming")
}

/// Published by rename, so the guest never sees a half-written one.
fn publish(path: &Path, contents: &str) {
  let partial = path.with_extension("partial");
  std::fs::write(&partial, contents).expect("write partial");
  std::fs::rename(&partial, path).expect("publish by rename");
}

fn wait_for(path: &Path, timeout: Duration, what: &str) {
  let deadline = Instant::now() + timeout;
  while !path.exists() {
    assert!(
      Instant::now() < deadline,
      "timed out waiting for {what} at {}",
      path.display()
    );
    std::thread::sleep(POLL);
  }
}

#[test]
fn host_sees_guest_appends_as_they_happen() {
  let directory = shared_directory();
  let stream = directory.join("stream");
  let eof = directory.join("stream.eof");
  let ready = directory.join("ready");

  std::fs::create_dir_all(&directory).expect("create shared directory");
  // A previous run's leftovers would let this one pass without a guest at all.
  for stale in [&stream, &eof, &ready] {
    let _ = std::fs::remove_file(stale);
  }

  println!("shared directory (host): {}", directory.display());
  publish(&ready, "");
  wait_for(&stream, START_TIMEOUT, "the guest to start appending");

  let started = Instant::now();
  // `reopen` is what `pump_stdin` does: a fresh open every poll, reading from the
  // offset already consumed. `held` keeps the descriptor it opened at first
  // sight. Both are measured because in the direction that fails, staleness
  // survives closing and re-opening — so a fresh open is not automatically the
  // safer of the two, and which one works has to be observed.
  let mut reopen = Observation::new("reopen");
  let mut held = Observation::new("held");
  let mut reopen_offset = 0u64;
  let mut held_file = File::open(&stream).expect("open stream");

  loop {
    // Checked before reading, so the read that follows sees everything written
    // before the marker appeared.
    let ended = eof.exists();
    let at = started.elapsed();

    if let Ok(mut file) = File::open(&stream) {
      let mut buffer = Vec::new();
      file
        .seek(SeekFrom::Start(reopen_offset))
        .expect("seek to consumed offset");
      file
        .read_to_end(&mut buffer)
        .expect("read from consumed offset");
      reopen_offset += buffer.len() as u64;
      reopen.record(at, buffer.len());
    }

    let mut buffer = Vec::new();
    held_file
      .read_to_end(&mut buffer)
      .expect("read from held descriptor");
    held.record(at, buffer.len());

    if ended {
      break;
    }

    assert!(
      started.elapsed() < RUN_TIMEOUT,
      "guest never marked the end of its input"
    );
    std::thread::sleep(POLL);
  }

  // The guest reports its own byte count, so the two halves do not have to agree
  // on a constant kept in two places.
  let claimed = std::fs::read_to_string(&eof).expect("read eof marker");
  let expected: usize = claimed
    .trim()
    .parse()
    .expect("eof marker holds a byte count");

  println!("guest wrote {expected} bytes; the host saw:");
  reopen.report();
  held.report();

  for stale in [&stream, &eof, &ready] {
    let _ = std::fs::remove_file(stale);
  }

  // The fatal case: a view that opened mid-write and stayed short forever, the
  // way the guest's view of host writes does.
  assert_eq!(
    reopen.total(),
    expected,
    "re-opening each poll lost {} of {expected} bytes — `pump_stdin` would truncate the guest's input",
    expected - reopen.total().min(expected)
  );
  // Everything arriving in one read at the end would still feed a command that
  // reads its input to EOF, but would make stdin useless for anything that
  // responds as it reads.
  assert!(
    reopen.batches.len() > 1,
    "the host saw all {expected} bytes in a single read, so guest input does not stream"
  );
  let first = reopen.batches.first().expect("at least one read").at;
  let last = reopen.batches.last().expect("at least one read").at;
  assert!(
    first < last,
    "every read landed at the same instant, so nothing was observed in flight"
  );

  // Not a failure: nothing reading guest input holds a descriptor across polls.
  // Recorded because it is the case most like the one that fails.
  if held.total() == expected {
    println!("held descriptor: also complete — a long-held view is not stale in this direction");
  } else {
    println!(
      "held descriptor: saw {} of {expected} bytes — a long-held view IS stale here; keep re-opening",
      held.total()
    );
  }
}
