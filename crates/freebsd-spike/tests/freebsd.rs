//! Runs the spike signed, since Virtualization refuses an unsigned binary.
//!
//! `cargo nextest run --run-ignored only --no-capture -E 'test(boots_freebsd)'`.
//! The first run downloads about 1 GB into `target/freebsd-spike/cache`.

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "boots a FreeBSD VM; run on purpose"]
fn boots_freebsd() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
  let work = root.join("target/freebsd-spike");
  std::fs::create_dir_all(&work).expect("make the work directory");

  // Signed as a copy, so cargo's binary is never rewritten.
  let signed = work.join("freebsd-spike-signed");
  std::fs::copy(env!("CARGO_BIN_EXE_freebsd-spike"), &signed).expect("copy the spike aside");
  let status = Command::new(root.join("bin/dev/sign"))
    .arg(&signed)
    .status()
    .expect("run bin/dev/sign");
  assert!(status.success(), "signing failed");

  let status = Command::new(&signed)
    .arg(&work)
    .status()
    .expect("run the spike");
  assert!(
    status.success(),
    "the spike failed; see {}",
    work.join("report.md").display()
  );
}
