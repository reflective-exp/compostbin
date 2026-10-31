//! The kernel is not published as an OCI image, so it comes from Kata
//! Containers' static release via `curl` and `tar`.

use super::files::{partial, settle};
use super::note;
use super::progress::Meter;
use super::store::{Store, initfs_version, is_older};
use crate::error::EngineError;
use containerization_framework::containerization::container::Mount;
use containerization_framework::containerization::image::Image;
use containerization_framework::containerization::image::image_store::GetInitImageOptions;
use containerization_framework::containerization::vm::SystemPlatform;
use containerization_framework::containerization_error::Code;
use containerization_framework::versions::{INIT_IMAGE, KERNEL_ARCHIVE_URL, KERNEL_IN_ARCHIVE};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn provision(store: &Store) -> Result<(), EngineError> {
  std::fs::create_dir_all(store.root()).map_err(|error| EngineError::failed("create the image store", error))?;

  kernel(store)?;
  initfs(store)?;
  retire(store);

  Ok(())
}

/// Leaves blobs to the build's reclaim, which runs once this build's blobs are tagged.
fn retire(store: &Store) {
  for path in store.retired() {
    match std::fs::remove_file(&path) {
      Ok(()) => note(&format!("removed {}", path.display())),
      Err(error) => note(&format!("could not remove {}: {error}", path.display())),
    }
  }

  let Ok(images) = store.images() else {
    return;
  };
  let Ok(listed) = images.list() else {
    return;
  };
  let references: Vec<String> = listed.iter().map(Image::reference).collect();

  for reference in retired_init_images(&references) {
    match images.delete(reference, false) {
      Ok(()) => note(&format!("removed {reference}")),
      Err(error) => note(&format!("could not remove {reference}: {error}")),
    }
  }
}

fn retired_init_images(references: &[String]) -> impl Iterator<Item = &str> {
  let repository = INIT_IMAGE
    .rsplit_once(':')
    .map_or(INIT_IMAGE, |(repository, _)| repository);

  references
    .iter()
    .map(String::as_str)
    .filter(move |reference| {
      reference
        .strip_prefix(repository)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|tag| is_older(tag, initfs_version()))
    })
}

pub fn initfs_mount(store: &Store) -> Mount {
  Mount::block("ext4", store.initfs().display().to_string(), "/", &["ro"], &[])
}

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

  // Unpacked aside and moved, so an interrupted provision never leaves a partial kernel.
  let scratch = partial(directory).map_err(|error| failed(&error))?;
  std::fs::create_dir(&scratch).map_err(|error| failed(&error))?;
  let fetched =
    fetch_kernel(&scratch).and_then(|kernel| std::fs::rename(&kernel, &destination).map_err(|error| failed(&error)));
  let _ = std::fs::remove_dir_all(&scratch);

  fetched
}

/// [`KERNEL_IN_ARCHIVE`] is a symlink to a versioned kernel beside it, so every kernel is unpacked and the link followed.
fn fetch_kernel(scratch: &Path) -> Result<PathBuf, EngineError> {
  let archive = scratch.join("kernel.tar.xz");

  note(&format!("downloading a kernel from {KERNEL_ARCHIVE_URL}"));
  run(
    Command::new("curl")
      .args(["--fail", "--silent", "--show-error", "--location", "--output"])
      .arg(&archive)
      .arg(KERNEL_ARCHIVE_URL),
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

  scratch
    .join(link)
    .canonicalize()
    .ok()
    .filter(|kernel| kernel.is_file())
    .ok_or_else(|| {
      EngineError::failed(
        "fetch a kernel",
        format!("{KERNEL_ARCHIVE_URL} holds no {KERNEL_IN_ARCHIVE}"),
      )
    })
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

/// Checks the store first only to announce the pull, which `get_init_image` does silently.
fn initfs(store: &Store) -> Result<(), EngineError> {
  let destination = store.initfs();

  if destination.is_file() {
    return Ok(());
  }

  let failed = |error: std::io::Error| EngineError::failed(format!("unpack {INIT_IMAGE}"), error);
  let directory = destination
    .parent()
    .expect("the init image is inside the store");
  std::fs::create_dir_all(directory).map_err(failed)?;

  let images = store.images()?;

  match images.get(INIT_IMAGE, false) {
    Err(error) if error.is_code(Code::NotFound) => note(&format!("pulling {INIT_IMAGE}")),
    Err(error) => return Err(error.into()),
    Ok(_) => {}
  }

  let meter = Meter::new();
  let image = images.get_init_image(
    INIT_IMAGE,
    GetInitImageOptions {
      progress: meter.handler(),
      ..Default::default()
    },
  )?;
  drop(meter);
  // Unpacked aside so concurrent first builds never share a partial file.
  let scratch = partial(directory).map_err(failed)?;
  let moved = image
    .init_block(&scratch, SystemPlatform::LINUX_ARM)
    .map_err(EngineError::from)
    .and_then(|_| settle(&scratch, &destination).map_err(failed));
  let _ = std::fs::remove_file(&scratch);

  moved
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn retires_only_older_tags_of_the_init_image() {
    let repository = INIT_IMAGE.rsplit_once(':').expect("a tagged init image").0;
    let references = [
      INIT_IMAGE.to_string(),
      format!("{repository}:0.0.1"),
      format!("{repository}:999.0.0"),
      format!("{repository}@sha256:aa"),
      format!("{repository}-other:0.0.1"),
      "compostbin/base:latest".to_string(),
    ];

    assert_eq!(
      retired_init_images(&references).collect::<Vec<_>>(),
      [references[1].as_str()]
    );
  }
}
