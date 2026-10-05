//! The file operations the store's caches share.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// A path in `directory` that nothing else will write to: where a file is made
/// before moving into place, so an interrupted write is never found, and
/// two processes writing the same file never share one.
pub fn partial(directory: &Path) -> io::Result<PathBuf> {
  let mut bytes = [0u8; 8];
  getrandom::fill(&mut bytes).map_err(io::Error::other)?;

  Ok(directory.join(format!("{:016x}.partial", u64::from_be_bytes(bytes))))
}

/// Whether `path` is a [`partial`]'s.
pub fn is_partial(path: &Path) -> bool {
  path
    .extension()
    .is_some_and(|extension| extension == "partial")
}

/// `clonefile` where supported, else a real copy. A clone costs only the blocks
/// either side later changes, so a multi-gigabyte rootfs is free to copy.
pub fn clone(source: &Path, destination: &Path) -> io::Result<()> {
  let _ = std::fs::remove_file(destination);

  let c_path = |path: &Path| CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other);
  let (source_c, destination_c) = (c_path(source)?, c_path(destination)?);

  // SAFETY: two NUL-terminated paths that outlive the call.
  if unsafe { libc::clonefile(source_c.as_ptr(), destination_c.as_ptr(), 0) } == 0 {
    return Ok(());
  }

  let error = io::Error::last_os_error();

  if !matches!(
    error.raw_os_error(),
    Some(libc::ENOTSUP | libc::EXDEV | libc::EINVAL | libc::EPERM)
  ) {
    return Err(error);
  }

  std::fs::copy(source, destination).map(|_| ())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_a_partial_no_one_else_will() {
    let directory = Path::new("/store");
    let first = partial(directory).expect("randomness");

    assert_ne!(first, partial(directory).expect("randomness"));
    assert_eq!(first.parent(), Some(directory));
    assert!(is_partial(&first));
    assert!(!is_partial(Path::new("/store/rootfs.ext4")));
  }

  #[test]
  fn clones_a_file_over_whatever_was_there() {
    let directory = tempfile::tempdir().expect("a temp dir");
    let source = directory.path().join("source");
    let destination = directory.path().join("destination");
    std::fs::write(&source, "rootfs").expect("a source");
    std::fs::write(&destination, "stale").expect("a destination");

    clone(&source, &destination).expect("a clone");

    assert_eq!(std::fs::read_to_string(&destination).expect("the clone"), "rootfs");
  }
}
