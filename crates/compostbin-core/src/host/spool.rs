//! The request/response directory tree, on whichever side of the mount the
//! caller happens to be.

use crate::error::{HostError, PathError};
use crate::host::request::Request;
use crate::host::{PARTIAL_SUFFIX, REQUEST_SUFFIX, REQUESTS_DIR, RESPONSES_DIR, RUNNING_DIR};
use std::io;
use std::path::{Path, PathBuf};

pub struct Spool {
  root: PathBuf,
}

impl Spool {
  pub fn new(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn root(&self) -> &Path {
    &self.root
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
      std::fs::create_dir_all(&directory).map_err(|source| PathError::new(&directory, source))?;
    }
    Ok(())
  }

  /// Clears whatever the last container left in flight, returning what it
  /// removed.
  ///
  /// A guest client removes its own files on the way out, but a killed one leaves
  /// chunks and a claim that nothing will ever collect. Creating the container is
  /// the safe moment to sweep them: every client that could be waiting died with
  /// the container before it.
  ///
  /// Requests are deliberately not swept — one submitted between `create` and the
  /// container starting is live, and dropping it would hang its client.
  pub fn sweep(&self) -> Result<Vec<PathBuf>, PathError> {
    let mut removed = Vec::new();

    for directory in [self.running(), self.responses()] {
      let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
        Err(source) => return Err(PathError::new(&directory, source)),
      };

      for entry in entries {
        let path = entry
          .map_err(|source| PathError::new(&directory, source))?
          .path();

        if path.is_dir() {
          continue;
        }

        std::fs::remove_file(&path).map_err(|source| PathError::new(&path, source))?;
        removed.push(path);
      }
    }

    removed.sort();
    Ok(removed)
  }

  /// Writes a request the way the guest does: `.partial` first, then rename.
  pub fn submit(&self, id: &str, request: &Request) -> Result<(), HostError> {
    let rendered = request.render()?;
    let partial = self.requests().join(format!("{id}{PARTIAL_SUFFIX}"));
    let final_path = self.requests().join(format!("{id}{REQUEST_SUFFIX}"));

    std::fs::write(&partial, rendered).map_err(|source| HostError::Io(PathError::new(&partial, source)))?;
    std::fs::rename(&partial, &final_path).map_err(|source| HostError::Io(PathError::new(&partial, source)))
  }

  /// The oldest unclaimed id. Ids are timestamp-prefixed, so name order is
  /// arrival order.
  pub(super) fn next_request_id(&self) -> Result<Option<String>, PathError> {
    let directory = self.requests();
    let entries = std::fs::read_dir(&directory).map_err(|source| PathError::new(&directory, source))?;

    let mut ids: Vec<String> = Vec::new();
    for entry in entries {
      let entry = entry.map_err(|source| PathError::new(&directory, source))?;
      let name = entry.file_name().to_string_lossy().into_owned();
      if let Some(id) = name.strip_suffix(REQUEST_SUFFIX) {
        ids.push(id.to_string());
      }
    }

    ids.sort();
    Ok(ids.into_iter().next())
  }
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
