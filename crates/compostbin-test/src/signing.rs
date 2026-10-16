use crate::lock::Lock;
use crate::output::stderr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

const SIGNED_NAME: &str = "compostbin-signed";
const SIGNED_SOURCE: &str = "compostbin-signed.source";
const SIGNING_LOCK: &str = "compostbin-signed.lock";

/// Virtualization.framework requires the `com.apple.security.virtualization`
/// entitlement, which every build drops; a copy is signed so no running binary
/// is ever rewritten.
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

  if std::fs::read_to_string(&marker).is_ok_and(|recorded| recorded == stamp) {
    return signed;
  }

  // Renamed into place so no test starts a half-written copy.
  let partial = target.join(format!("{SIGNED_NAME}.partial"));
  std::fs::copy(&unsigned, &partial).expect("copy the binary aside");
  sign(&partial);
  std::fs::rename(&partial, &signed).expect("publish the signed binary");
  std::fs::write(&marker, &stamp).expect("record what was signed");

  signed
}

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

pub(crate) fn target_dir() -> PathBuf {
  let exe = std::env::current_exe().expect("the running binary has a path");
  let dir = exe.parent().expect("the running binary is in a directory");

  // A test binary is under `target/<profile>/deps`; `prepare-store` is beside
  // `compostbin`.
  if dir.ends_with("deps") {
    dir.parent().expect("deps is under the profile directory")
  } else {
    dir
  }
  .to_path_buf()
}

fn repository_root() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .expect("canonical repository root")
}
