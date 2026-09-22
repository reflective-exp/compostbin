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
//! Both halves are this test's: it starts the appender in a session of its own
//! and reads the file through the mount. The host publishes `ready` before
//! looking, so its first look at `stream` lands mid-write; a later look would
//! only measure whole-file propagation, which already works.

use compostbin_test::{Project, stderr, stdout};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Finer than the guest's write interval, to catch the file mid-write.
const POLL: Duration = Duration::from_millis(25);
/// The guest half waits for a container of its own first.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// A hung guest must fail rather than hang the suite.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// The guest half: appends fixed-width lines on a schedule, then says how many
/// bytes it wrote. Long enough to span many host polls, short enough to wait
/// through.
///
/// It waits for a line on its stdin so that the host's first read lands
/// mid-write, and publishes the byte count by rename so the marker never
/// appears half-written. Stdin rather than a file the host writes: the guest's
/// view of the mount is the stale direction, which is what this measures.
const APPENDER: &str = r#"#!/bin/sh
set -eu

shared="$(cd "$(dirname "$0")" && pwd)"
stream="$shared/stream"
lines=24
interval=0.1

read ready

: > "$stream"
written=0
sequence=1
while [ "$sequence" -le "$lines" ]; do
  # Fixed width, so the byte count is exact without stat-ing the file under
  # test.
  line=$(printf 'line %04d of %04d' "$sequence" "$lines")
  printf '%s\n' "$line" >> "$stream"
  written=$((written + ${#line} + 1))
  sequence=$((sequence + 1))
  sleep "$interval"
done

printf '%s\n' "$written" > "$shared/eof.partial"
mv "$shared/eof.partial" "$stream.eof"
"#;

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
fn appends_reach_the_host_as_they_happen() {
  let project = Project::new("cbt-streaming");
  project.write("appender.sh", APPENDER);

  let directory: PathBuf = project.dir().to_path_buf();
  let stream = directory.join("stream");
  let eof = directory.join("stream.eof");

  // The guest waits to be told to start, so this side is watching from the
  // first byte it writes.
  let mut appender = project.guest_in_background("sh appender.sh");

  println!("shared directory (host): {}", directory.display());
  appender.send("go");
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

  let finished = appender.finish();
  assert!(
    finished.status.success(),
    "the guest half failed: {}{}",
    stdout(&finished),
    stderr(&finished)
  );

  println!("guest wrote {expected} bytes; the host saw:");
  reopen.report();
  held.report();

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
