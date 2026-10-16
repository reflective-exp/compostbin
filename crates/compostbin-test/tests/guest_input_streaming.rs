#![cfg(feature = "integration")]
//! Measures whether the host sees a guest's appends to a mounted file as they
//! happen, which `pump_stdin` relies on; host-to-guest appends stay stale.

use compostbin_test::{Project, poll_until, stderr, stdout};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

/// Finer than the guest's write interval, to catch the file mid-write.
const POLL: Duration = Duration::from_millis(25);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Started over stdin rather than a host-written file, since the guest's view
/// of host writes is the stale direction.
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

struct Batch {
  at: Duration,
  bytes: usize,
}

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

#[test]
fn appends_reach_the_host_as_they_happen() {
  let project = Project::new("cbt-streaming");
  project.write("appender.sh", APPENDER);

  let directory = project.dir();
  let stream = directory.join("stream");
  let eof = directory.join("stream.eof");

  let mut appender = project.guest_in_background("sh appender.sh");

  println!("shared directory (host): {}", directory.display());
  appender.send("go");
  poll_until(START_TIMEOUT, "the guest to start appending", || {
    stream.exists().then_some(())
  });

  let started = Instant::now();
  // `reopen` mirrors `pump_stdin`; `held` keeps its first descriptor.
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

  assert_eq!(
    reopen.total(),
    expected,
    "re-opening each poll lost {} of {expected} bytes — `pump_stdin` would truncate the guest's input",
    expected - reopen.total().min(expected)
  );
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

  if held.total() == expected {
    println!("held descriptor: also complete — a long-held view is not stale in this direction");
  } else {
    println!(
      "held descriptor: saw {} of {expected} bytes — a long-held view IS stale here; keep re-opening",
      held.total()
    );
  }
}
