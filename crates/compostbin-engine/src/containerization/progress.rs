//! A pull's progress, as a line on stderr redrawn in place.
//!
//! Only on a terminal: anything else reading stderr gets the build log's lines
//! and no redraws.
//!
//! Not an unpack's: asked for progress, `EXT4Unpacker` decompresses every
//! layer an extra time to total it.

use super::stdio::is_tty;
use containerization_framework::containerization_extras::{ProgressEvent, ProgressHandler};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Redraws no more often than this; Swift reports every buffer it writes.
const REDRAW_EVERY: Duration = Duration::from_millis(100);

/// Moves to the start of the line and clears it.
const CLEAR_LINE: &str = "\r\x1b[2K";

/// What has been reported so far.
#[derive(Debug, Default)]
struct Tally {
  items: i64,
  total_items: i64,
  size: i64,
  total_size: i64,
}

impl Tally {
  fn add(&mut self, events: &[ProgressEvent]) {
    for event in events {
      match *event {
        ProgressEvent::AddItems(items) => self.items += items as i64,
        ProgressEvent::AddTotalItems(items) => self.total_items += items as i64,
        ProgressEvent::AddSize(size) => self.size += size,
        ProgressEvent::AddTotalSize(size) => self.total_size += size,
      }
    }
  }

  /// The progress line. `None` before any total.
  fn line(&self) -> Option<String> {
    let items = (self.total_items > 0).then(|| format!("{} of {} blobs", self.items, self.total_items));

    if self.total_size <= 0 {
      return items;
    }

    let percent = (self.size.saturating_mul(100) / self.total_size).min(100);
    let size = format!("{percent}% ({} of {})", bytes(self.size), bytes(self.total_size));

    Some(match items {
      Some(items) => format!("{size}, {items}"),
      None => size,
    })
  }
}

/// `size` in decimal units, as Finder shows it.
fn bytes(size: i64) -> String {
  const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];

  if size < 1000 {
    return format!("{size} B");
  }

  let mut scaled = size as f64 / 1000.0;
  let mut unit = 0;

  while scaled >= 1000.0 && unit < UNITS.len() - 1 {
    scaled /= 1000.0;
    unit += 1;
  }

  format!("{scaled:.1} {}", UNITS[unit])
}

struct Drawn {
  tally: Tally,
  /// When the line was last drawn; `None` until it first is.
  at: Option<Instant>,
}

/// A progress line, erased when dropped. Inert off a terminal.
pub struct Meter {
  drawn: Option<Arc<Mutex<Drawn>>>,
}

impl Meter {
  pub fn new() -> Self {
    let drawn = is_tty(libc::STDERR_FILENO).then(|| {
      Arc::new(Mutex::new(Drawn {
        tally: Tally::default(),
        at: None,
      }))
    });

    Self { drawn }
  }

  /// What to hand the framework: `None` off a terminal.
  pub fn handler(&self) -> Option<ProgressHandler> {
    let drawn = Arc::clone(self.drawn.as_ref()?);

    Some(Box::new(move |events| {
      // Held while drawing, so concurrent batches never interleave a line.
      let mut drawn = super::lock(&drawn);
      drawn.tally.add(events);

      if drawn.at.is_some_and(|at| at.elapsed() < REDRAW_EVERY) {
        return;
      }

      if let Some(line) = drawn.tally.line() {
        eprint!("{CLEAR_LINE}{line}");
        drawn.at = Some(Instant::now());
      }
    }))
  }
}

impl Drop for Meter {
  fn drop(&mut self) {
    if let Some(drawn) = &self.drawn
      && super::lock(drawn).at.is_some()
    {
      eprint!("{CLEAR_LINE}");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tally(events: &[ProgressEvent]) -> Tally {
    let mut tally = Tally::default();
    tally.add(events);
    tally
  }

  #[test]
  fn shows_nothing_before_a_total() {
    assert_eq!(tally(&[ProgressEvent::AddSize(10)]).line(), None);
  }

  #[test]
  fn shows_bytes_and_items_against_their_totals() {
    let tally = tally(&[
      ProgressEvent::AddTotalSize(2_500_000_000),
      ProgressEvent::AddTotalItems(7),
      ProgressEvent::AddSize(1_000_000_000),
      ProgressEvent::AddSize(125_000_000),
      ProgressEvent::AddItems(3),
    ]);

    assert_eq!(tally.line().as_deref(), Some("45% (1.1 GB of 2.5 GB), 3 of 7 blobs"));
  }

  #[test]
  fn shows_items_alone_without_a_total_size() {
    let tally = tally(&[ProgressEvent::AddTotalItems(4), ProgressEvent::AddItems(1)]);

    assert_eq!(tally.line().as_deref(), Some("1 of 4 blobs"));
  }

  #[test]
  fn adds_totals_reported_in_stages() {
    // A pull totals the index, then its manifests, then their layers.
    let tally = tally(&[
      ProgressEvent::AddTotalSize(500),
      ProgressEvent::AddSize(500),
      ProgressEvent::AddTotalSize(1500),
    ]);

    assert_eq!(tally.line().as_deref(), Some("25% (500 B of 2.0 kB)"));
  }

  #[test]
  fn never_shows_more_than_all_of_it() {
    let tally = tally(&[ProgressEvent::AddTotalSize(100), ProgressEvent::AddSize(150)]);

    assert_eq!(tally.line().as_deref(), Some("100% (150 B of 100 B)"));
  }

  #[test]
  fn scales_bytes_to_the_largest_unit_under_a_thousand() {
    assert_eq!(bytes(999), "999 B");
    assert_eq!(bytes(1000), "1.0 kB");
    assert_eq!(bytes(12_345_678), "12.3 MB");
    assert_eq!(bytes(4_000_000_000_000_000), "4000.0 TB");
  }
}
