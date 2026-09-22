//! Stand-ins for every platform but macOS: the same surface as the real types,
//! failing on first use of anything that needs a VM, so a cross-platform
//! workspace still compiles.
//!
//! Every method the macOS types have belongs here too, or a caller builds on
//! macOS and stops building elsewhere.

use crate::error::Error;
use crate::model::{BootSpec, BuildPlan, ExecRequest};
use crate::store::{Store, StoreError};
use std::os::fd::RawFd;

fn unsupported() -> Error {
  Error::unavailable("use Containerization.framework", "it is macOS only")
}

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

  pub fn boot(&self, _spec: &BootSpec) -> Result<(), Error> {
    Err(unsupported())
  }

  pub fn exec(&self, _request: &ExecRequest) -> Result<i32, Error> {
    Err(unsupported())
  }

  pub fn resize(&self, _id: &str, _terminal: RawFd) -> Result<(), Error> {
    Err(unsupported())
  }

  /// Nothing can be running, since nothing can boot.
  pub fn is_running(&self, _name: &str) -> bool {
    false
  }

  pub fn is_unpacked(&self, _image: &str) -> Result<bool, Error> {
    Err(unsupported())
  }

  /// The store is plain files, so this answers here as it does on macOS.
  pub fn images(&self) -> Result<Vec<String>, StoreError> {
    self.store.images()
  }

  pub fn version() -> String {
    format!(
      "Containerization {}, kernel {}",
      crate::INITFS_VERSION,
      crate::KERNEL_VERSION
    )
  }
}

pub struct Builder {
  store: Store,
}

impl Builder {
  pub fn new(store: Store) -> Self {
    Self { store }
  }

  pub fn store(&self) -> &Store {
    &self.store
  }

  pub fn build(&self, _plan: &BuildPlan) -> Result<(), Error> {
    Err(unsupported())
  }

  pub fn provision(&self) -> Result<(), Error> {
    Err(unsupported())
  }
}
