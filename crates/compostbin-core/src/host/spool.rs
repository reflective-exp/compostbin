//! The request/response directory tree, from either side of the mount.

use crate::error::{At, HostError, PathError};
use crate::host::request::Request;
use crate::host::{PARTIAL_SUFFIX, REQUEST_SUFFIX, REQUESTS_DIR, RESPONSES_DIR, RUNNING_DIR};
use std::path::{Path, PathBuf};

pub struct Spool {
  root: PathBuf,
}

impl Spool {
  pub fn new(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn requests(&self) -> PathBuf {
    self.root.join(REQUESTS_DIR)
  }

  pub fn running(&self) -> PathBuf {
    self.root.join(RUNNING_DIR)
  }

  pub fn responses(&self) -> PathBuf {
    self.root.join(RESPONSES_DIR)
  }

  /// Called on the host before the container starts: the guest cannot create
  /// these itself on a mount that does not yet exist.
  pub fn create(&self) -> Result<(), PathError> {
    for directory in [self.requests(), self.running(), self.responses()] {
      std::fs::create_dir_all(&directory).at(&directory)?;
    }
    Ok(())
  }

  /// Empties the spool without unlinking it, and recreates whatever is missing.
  ///
  /// The directories must survive: a running container's mount is attached to
  /// the inode, so a spool removed and recreated at the same path leaves the
  /// guest holding a dead directory for as long as the container lives.
  pub fn empty(&self) -> Result<(), PathError> {
    self.create()?;

    for directory in [self.requests(), self.running(), self.responses()] {
      for entry in std::fs::read_dir(&directory).at(&directory)? {
        let path = entry.at(&directory)?.path();

        std::fs::remove_file(&path).at(&path)?;
      }
    }

    Ok(())
  }

  /// Clears whatever the last container left in flight, returning what it
  /// removed.
  ///
  /// A killed guest client leaves chunks and a claim nothing will collect.
  /// Creating the container is the safe moment to sweep them: every client that
  /// could be waiting died with the previous container.
  ///
  /// Requests are not swept: one submitted between `create` and the container
  /// starting is live, and dropping it would hang its client.
  pub fn sweep(&self) -> Result<Vec<PathBuf>, PathError> {
    let mut removed = Vec::new();

    for directory in [self.running(), self.responses()] {
      let entries = match std::fs::read_dir(&directory).at(&directory) {
        Ok(entries) => entries,
        Err(error) if error.is_not_found() => continue,
        Err(error) => return Err(error),
      };

      for entry in entries {
        let path = entry.at(&directory)?.path();

        if path.is_dir() {
          continue;
        }

        std::fs::remove_file(&path).at(&path)?;
        removed.push(path);
      }
    }

    removed.sort();
    Ok(removed)
  }

  /// Writes a request the way the guest does: `.partial` first, then rename.
  pub fn submit(&self, id: &str, request: &Request) -> Result<(), HostError> {
    let rendered = request.render()?;
    Ok(publish(&self.requests(), &format!("{id}{REQUEST_SUFFIX}"), rendered)?)
  }

  /// The oldest unclaimed id. Ids are timestamp-prefixed, so name order is
  /// arrival order.
  pub(super) fn next_request_id(&self) -> Result<Option<String>, PathError> {
    let directory = self.requests();

    let mut ids: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&directory).at(&directory)? {
      let name = entry.at(&directory)?.file_name();
      let name = name.to_string_lossy();

      if let Some(id) = name.strip_suffix(REQUEST_SUFFIX) {
        ids.push(id.to_string());
      }
    }

    Ok(ids.into_iter().min())
  }
}

/// Writes `<name>.partial`, then renames it to `name`, so a reader across the
/// mount never finds the file incomplete.
pub(super) fn publish(directory: &Path, name: &str, contents: impl AsRef<[u8]>) -> Result<(), PathError> {
  let partial = directory.join(format!("{name}{PARTIAL_SUFFIX}"));

  std::fs::write(&partial, contents).at(&partial)?;
  std::fs::rename(&partial, directory.join(name)).at(&partial)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::fixtures::spool;
  use crate::host::{OUTPUT_STREAM, STATUS_SUFFIX};
  use tempfile::TempDir;

  /// Creating a container is the moment a killed client's leftovers are provably
  /// dead.
  #[test]
  fn sweeps_what_a_killed_client_left_behind() {
    let (_temp, spool) = spool();
    std::fs::write(spool.running().join("0001"), "greet\n").expect("write claim");
    std::fs::write(
      spool
        .responses()
        .join(format!("0001.{OUTPUT_STREAM}.000001")),
      "hello\n",
    )
    .expect("write chunk");
    std::fs::write(spool.responses().join(format!("0001{STATUS_SUFFIX}")), "0\n").expect("write status");

    assert_eq!(spool.sweep().expect("sweep should succeed").len(), 3);
    assert_eq!(
      std::fs::read_dir(spool.responses())
        .expect("responses")
        .count(),
      0
    );
    assert_eq!(std::fs::read_dir(spool.running()).expect("running").count(), 0);
  }

  /// A request submitted between the sweep and the container starting is live;
  /// dropping it would hang the client waiting on its status.
  #[test]
  fn sweeps_nothing_a_client_is_still_waiting_on() {
    let (_temp, spool) = spool();
    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");

    assert_eq!(spool.sweep().expect("sweep should succeed"), Vec::<PathBuf>::new());
    assert!(
      spool
        .requests()
        .join(format!("0001{REQUEST_SUFFIX}"))
        .exists()
    );
  }

  #[test]
  fn sweeping_a_spool_that_was_never_created_is_not_an_error() {
    let temp = TempDir::new().expect("temp dir");

    assert_eq!(
      Spool::new(temp.path().join("absent"))
        .sweep()
        .expect("sweep should succeed"),
      Vec::<PathBuf>::new()
    );
  }
}
