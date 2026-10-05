//! Putting a kernel and an init image in the store.
//!
//! A VM needs a kernel to boot and an init image to boot into. Both are fetched:
//!
//! - the init image is an OCI image, pulled through `ImageStore` and unpacked
//!   to a block of its own;
//! - the kernel is not published as an image, so it comes from Kata Containers'
//!   static release, as in Containerization's Makefile, through `curl` and
//!   `tar`.
//!
//! Idempotent and cheap when there is nothing to do, so `build` calls it every
//! time.

use super::files::partial;
use super::note;
use super::store::{INITFS_REFERENCE, KERNEL_IN_ARCHIVE, KERNEL_URL, Store};
use crate::error::EngineError;
use containerization_framework::containerization::{Mount, SystemPlatform};
use std::path::Path;
use std::process::Command;

pub fn provision(store: &Store) -> Result<(), EngineError> {
  std::fs::create_dir_all(store.root()).map_err(|error| EngineError::failed("create the image store", error))?;

  kernel(store)?;
  initfs(store)
}

/// The init image, read-only, as `ContainerManager(initfs:)` boots it.
pub fn initfs_mount(store: &Store) -> Mount {
  Mount::block("ext4", store.initfs().display().to_string(), "/", &["ro"])
}

/// Downloads and unpacks the kernel, unless it is already there.
fn kernel(store: &Store) -> Result<(), EngineError> {
  let destination = store.kernel();

  if destination.is_file() {
    return Ok(());
  }

  let failed = |error: &dyn std::fmt::Display| EngineError::failed("fetch a kernel", error);
  let directory = destination
    .parent()
    .expect("the kernel is inside the store");
  std::fs::create_dir_all(directory).map_err(|error| failed(&error))?;

  // Unpacked aside and moved, so an interrupted provision never leaves a
  // half-written kernel that looks complete.
  let scratch = partial(directory).map_err(|error| failed(&error))?;
  std::fs::create_dir(&scratch).map_err(|error| failed(&error))?;
  let fetched =
    fetch_kernel(&scratch).and_then(|kernel| std::fs::rename(&kernel, &destination).map_err(|error| failed(&error)));
  let _ = std::fs::remove_dir_all(&scratch);

  fetched
}

/// Downloads [`KERNEL_URL`] into `scratch` and returns where its kernel landed.
///
/// To a file, not a pipe: the archive is hundreds of megabytes and `tar` reads
/// only the kernels out of it. In Kata's release [`KERNEL_IN_ARCHIVE`] links to
/// the versioned kernel beside it, so every kernel there is unpacked and the
/// link followed.
fn fetch_kernel(scratch: &Path) -> Result<std::path::PathBuf, EngineError> {
  let archive = scratch.join("kernel.tar.xz");

  note(&format!("downloading a kernel from {KERNEL_URL}"));
  run(
    Command::new("curl")
      .args(["--fail", "--silent", "--show-error", "--location", "--output"])
      .arg(&archive)
      .arg(KERNEL_URL),
  )?;

  let link = Path::new(KERNEL_IN_ARCHIVE);
  let kernels = link.parent().expect("the kernel is in a directory");

  note(&format!("unpacking {KERNEL_IN_ARCHIVE}"));
  run(
    Command::new("tar")
      .arg("-xf")
      .arg(&archive)
      .arg("-C")
      .arg(scratch)
      .arg("--include")
      .arg(format!("*{}/vmlinux*", kernels.display())),
  )?;

  let unpacked = scratch.join(link);

  unpacked
    .canonicalize()
    .ok()
    .filter(|kernel| kernel.is_file())
    .ok_or_else(|| EngineError::failed("fetch a kernel", format!("{KERNEL_URL} holds no {KERNEL_IN_ARCHIVE}")))
}

fn run(command: &mut Command) -> Result<(), EngineError> {
  let program = command.get_program().to_string_lossy().into_owned();
  let status = command
    .status()
    .map_err(|error| EngineError::unavailable(format!("run {program}"), error))?;

  if !status.success() {
    return Err(EngineError::failed("fetch a kernel", format!("{program} {status}")));
  }

  Ok(())
}

/// Pulls and unpacks the init image, unless it is already there.
///
/// `get_init_image` pulls on its own; checking first lets it announce the pull,
/// the slow part of a first build, which would otherwise look like a hang.
fn initfs(store: &Store) -> Result<(), EngineError> {
  let destination = store.initfs();

  if destination.is_file() {
    return Ok(());
  }

  let failed = |error: std::io::Error| EngineError::failed(format!("unpack {INITFS_REFERENCE}"), error);
  let directory = destination
    .parent()
    .expect("the init image is inside the store");
  std::fs::create_dir_all(directory).map_err(failed)?;

  let images = store.images()?;

  if images.get(INITFS_REFERENCE, false).is_err() {
    note(&format!("pulling {INITFS_REFERENCE}"));
  }

  let image = images.get_init_image(INITFS_REFERENCE)?;
  // Unpacked aside and moved into place: no half-written file, no sharing
  // between concurrent first builds.
  let scratch = partial(directory).map_err(failed)?;
  let unpacked = image.init_block(&scratch, SystemPlatform::LINUX_ARM);
  let moved = unpacked.map_err(EngineError::from).and_then(|_| {
    match std::fs::rename(&scratch, &destination) {
      // Another build unpacked it first; its copy is equivalent.
      Err(_) if destination.is_file() => Ok(()),
      moved => moved.map_err(failed),
    }
  });
  let _ = std::fs::remove_file(&scratch);

  moved
}
