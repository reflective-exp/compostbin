//! Booting a container, and running processes in it.
//!
//! A container lives *in* this process, not a daemon, so it exists exactly as
//! long as the process that booted it. Nothing lists containers: ask
//! [`Session::is_running`], which answers for this process alone.
//!
//! Reaching a container from a second process is the caller's to arrange — this
//! crate hands a guest process whichever descriptors it is given, wherever they
//! came from.

use crate::error::Error;
use crate::model::{BootSpec, ExecRequest};
use crate::store::{INITFS_REFERENCE, Store, StoreError};
use crate::{checked, ffi, wire};
use std::os::fd::RawFd;

pub struct Session {
  store: Store,
}

impl Session {
  pub fn new(store: Store) -> Self {
    Self { store }
  }

  pub fn store(&self) -> &Store {
    &self.store
  }

  /// Boots `spec`'s VM and leaves it running, owned by this process.
  ///
  /// The container's directory is cleared first: the VM dies with its process,
  /// so nothing cleaned up after the previous run, and a container always
  /// starts from a fresh clone of its image's unpacked rootfs.
  pub fn boot(&self, spec: &BootSpec) -> Result<(), Error> {
    let _ = std::fs::remove_dir_all(self.store.container_dir(&spec.name));

    let code = ffi::czbridge_boot(
      &spec.name,
      &self.store.root().display().to_string(),
      &self.store.kernel().display().to_string(),
      INITFS_REFERENCE,
      &spec.image,
      spec.resources.cpus as i32,
      spec.resources.memory_in_bytes,
      spec.rootfs_capacity_in_bytes,
      &wire::lines(&wire::mounts(&spec.mounts)),
      &wire::lines(&wire::sockets(&spec.sockets)),
      &wire::lines(&spec.environment),
      &wire::lines(&spec.arguments),
      &wire::working_directory(spec.workdir.as_deref()),
      &spec.network.ipv4_address,
      &spec.network.ipv4_gateway,
    );

    checked(code)
      .map(|_| ())
      .map_err(|error| Error::failed(format!("boot {}", spec.name), error))
  }

  /// Runs a process in a booted container and blocks until it exits, returning
  /// its exit code.
  ///
  /// The descriptors in `request.stdio` are closed when the process ends, so
  /// pass [`crate::Stdio::try_clone`]'s result to keep your own streams open.
  pub fn exec(&self, request: &ExecRequest) -> Result<i32, Error> {
    let code = ffi::czbridge_exec(
      &request.name,
      &request.id,
      &wire::lines(&request.arguments),
      &wire::lines(&request.environment),
      request.user.as_deref().unwrap_or(""),
      &wire::working_directory(request.workdir.as_deref()),
      request.term.as_deref().unwrap_or(""),
      request.stdio.terminal,
      request.stdio.stdin,
      request.stdio.stdout,
      request.stdio.stderr,
    );

    checked(code).map_err(|error| Error::failed("exec", error))
  }

  /// Tells the guest that the terminal `id`'s process reads changed size.
  ///
  /// A no-op once the process has gone, since the window may change size as it
  /// exits. `terminal` is re-read rather than passed as a size, so a stale one
  /// cannot race a second resize.
  pub fn resize(&self, id: &str, terminal: RawFd) -> Result<(), Error> {
    checked(ffi::czbridge_resize(id, terminal))
      .map(|_| ())
      .map_err(|error| Error::failed(format!("resize {id}"), error))
  }

  /// Whether *this process* owns a running container by that name.
  ///
  /// Only this process can say: a container is not registered anywhere outside
  /// the process that booted it.
  pub fn is_running(&self, name: &str) -> bool {
    ffi::czbridge_is_running(name)
  }

  /// Whether [`Session::boot`] can skip unpacking this image. False until its
  /// first (slow) boot, which a caller may want to announce.
  pub fn is_unpacked(&self, image: &str) -> Result<bool, Error> {
    let code = ffi::czbridge_is_unpacked(&self.store.root().display().to_string(), image);

    Ok(checked(code).map_err(|error| Error::failed("find the image", error))? == 1)
  }

  /// Every image the store holds, as `name:tag`. Read from the store's index;
  /// there is no daemon to ask.
  pub fn images(&self) -> Result<Vec<String>, StoreError> {
    self.store.images()
  }

  /// The pinned Containerization release and kernel, which are pinned
  /// separately and whose mismatch would fail only at runtime.
  pub fn version() -> String {
    format!(
      "Containerization {}, kernel {}",
      crate::INITFS_VERSION,
      crate::KERNEL_VERSION
    )
  }
}
