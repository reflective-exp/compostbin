#![cfg(feature = "integration")]
//! Measures guest-to-host streaming over a bind mount: the guest appends to a
//! file while the host reads it.
//!
//! The other direction doesn't work: a file the guest opens while the host is
//! appending stays permanently stale in the guest. So command output travels as
//! numbered chunks, each renamed into place complete.
//!
//! Guest input relies on the asymmetry: the guest appends and `pump_stdin`
//! reads as it arrives. This measures that rather than assuming it.
//!
//! Run from inside a session via `bin/test/guest-input-streaming`, which starts
//! the guest half and asks the host to run this. Alone, it fails waiting for
//! the guest — half a measurement is worse than none.
//!
//! The halves rendezvous in `target/guest-input-streaming` over the repo mount.
//! The host publishes `ready` before looking, so its first look at `stream`
//! lands mid-write; a later look would only measure whole-file propagation,
//! which already works.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Finer than the guest's write interval, to catch the file mid-write.
const POLL: Duration = Duration::from_millis(25);
/// The guest half is started by hand.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// A hung guest must fail rather than hang the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// One non-empty read.
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
  // Leftovers would let this pass without a guest.
  for stale in [&stream, &eof, &ready] {
    let _ = std::fs::remove_file(stale);
  }

  println!("shared directory (host): {}", directory.display());
  publish(&ready, "");
  wait_for(&stream, START_TIMEOUT, "the guest to start appending");

  let started = Instant::now();
  // `reopen` mirrors `pump_stdin`: fresh open each poll, from the consumed
  // offset. `held` keeps its first descriptor. Both are measured because in the
  // failing direction staleness survives re-opening, so neither is presumed
  // safer.
  let mut reopen = Observation::new("reopen");
  let mut held = Observation::new("held");
  let mut reopen_offset = 0u64;
  let mut held_file = File::open(&stream).expect("open stream");

  loop {
    // Checked before reading, so this read covers everything before the marker.
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

  // The guest reports its byte count, so no constant is duplicated.
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

  // The fatal case: opened mid-write and stayed short forever, as the guest's
  // view of host writes does.
  assert_eq!(
    reopen.total(),
    expected,
    "re-opening each poll lost {} of {expected} bytes — `pump_stdin` would truncate the guest's input",
    expected - reopen.total().min(expected)
  );
  // One read at the end would suffice for read-to-EOF commands but break
  // anything interactive.
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

  // Informational: nothing holds a descriptor across polls, but this is the
  // case closest to the one that fails.
  if held.total() == expected {
    println!("held descriptor: also complete — a long-held view is not stale in this direction");
  } else {
    println!(
      "held descriptor: saw {} of {expected} bytes — a long-held view IS stale here; keep re-opening",
      held.total()
    );
  }
}
