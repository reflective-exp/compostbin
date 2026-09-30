//! The binary under test, copied aside and given the entitlement it needs.

use crate::lock::Lock;
use crate::output::stderr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// The entitled copy of the binary under test, beside the one cargo builds.
const SIGNED_NAME: &str = "compostbin-signed";
/// Records which build `SIGNED_NAME` was copied from, so a rebuilt binary is
/// signed again and an unchanged one is not.
const SIGNED_SOURCE: &str = "compostbin-signed.source";
/// Held while one process signs. Taken by creating it, so the loser waits
/// rather than signing over a copy another test is about to run.
const SIGNING_LOCK: &str = "compostbin-signed.lock";

/// The binary under test, copied aside and signed.
///
/// Virtualization.framework refuses every call from a binary without
/// `com.apple.security.virtualization`, and the build cargo just did dropped
/// whatever signature the last one had. Signing the copy rather than the
/// original leaves the developer's `target/debug/compostbin` alone and, more to
/// the point, never rewrites a file another test is in the middle of running.
pub fn signed_binary() -> PathBuf {
  let target = target_dir();
  let unsigned = target.join("compostbin");
  let signed = target.join(SIGNED_NAME);
  let marker = target.join(SIGNED_SOURCE);

  assert!(
    unsigned.exists(),
    "{} has not been built; run the whole suite (`cargo nextest run --features compostbin-test/integration`) so cargo builds it",
    unsigned.display()
  );

  let stamp = stamp(&unsigned);
  if std::fs::read_to_string(&marker).is_ok_and(|recorded| recorded == stamp) {
    return signed;
  }

  let _lock = Lock::take(target.join(SIGNING_LOCK));

  // The process that held the lock may have just done this.
  if std::fs::read_to_string(&marker).is_ok_and(|recorded| recorded == stamp) {
    return signed;
  }

  // Signed under another name and renamed into place, so a test starting the
  // binary either gets the last complete one or this one, never a half-written
  // copy — and a process already running the old one keeps its own inode.
  let partial = target.join(format!("{SIGNED_NAME}.partial"));
  std::fs::copy(&unsigned, &partial).expect("copy the binary aside");
  sign(&partial);
  std::fs::rename(&partial, &signed).expect("publish the signed binary");
  std::fs::write(&marker, &stamp).expect("record what was signed");

  signed
}

/// What a build of the binary is: its size and when it was written. Cheaper
/// than hashing it, and a rebuild changes both.
fn stamp(binary: &Path) -> String {
  let metadata = std::fs::metadata(binary).expect("the binary should be readable");
  let modified = metadata
    .modified()
    .expect("modification time")
    .duration_since(SystemTime::UNIX_EPOCH)
    .expect("the binary is not older than the epoch");

  format!("{} {}", metadata.len(), modified.as_nanos())
}

fn sign(binary: &Path) {
  let script = repository_root().join("bin/dev/sign");
  let signed = Command::new(&script)
    .arg(binary)
    .output()
    .unwrap_or_else(|error| panic!("{} should run: {error}", script.display()));

  assert!(
    signed.status.success(),
    "signing {} failed: {}",
    binary.display(),
    stderr(&signed)
  );
}

/// The directory cargo built into: `…/target/<profile>`, two above this test
/// binary in `…/target/<profile>/deps`.
pub(crate) fn target_dir() -> PathBuf {
  std::env::current_exe()
    .expect("the test binary has a path")
    .parent()
    .and_then(Path::parent)
    .expect("the test binary is under target/<profile>/deps")
    .to_path_buf()
}

fn repository_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .expect("canonical repository root")
}
