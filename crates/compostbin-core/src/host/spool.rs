use crate::error::{At, PathError};
use crate::host::{
  ERROR_STREAM, PARTIAL_SUFFIX, REQUEST_SUFFIX, REQUESTS_DIR, RESPONSES_DIR, RUNNING_DIR, SEQUENCE_WIDTH, STATUS_SUFFIX,
};
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

  pub(super) fn running(&self) -> PathBuf {
    self.root.join(RUNNING_DIR)
  }

  pub fn responses(&self) -> PathBuf {
    self.root.join(RESPONSES_DIR)
  }

  /// Must run before the container starts, since the guest cannot create these.
  pub fn create(&self) -> Result<(), PathError> {
    for directory in [self.requests(), self.running(), self.responses()] {
      std::fs::create_dir_all(&directory).at(&directory)?;
    }
    Ok(())
  }

  /// Never unlinks the directories, because a running container's mount is
  /// bound to their inodes.
  pub fn empty(&self) -> Result<(), PathError> {
    self.create()?;

    for directory in [self.requests(), self.running(), self.responses()] {
      crate::fs::empty(&directory)?;
    }

    Ok(())
  }

  /// Safe only when creating a container; requests are left alone because one
  /// submitted before the container starts is still live.
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

  /// The rename is the claim, so racing agents cannot both win.
  pub(super) fn claim_next(&self) -> Result<Option<(String, PathBuf)>, PathError> {
    let Some(id) = self.next_request_id()? else {
      return Ok(None);
    };

    let submitted = self.requests().join(format!("{id}{REQUEST_SUFFIX}"));
    let claimed = self.running().join(&id);

    if std::fs::rename(&submitted, &claimed).is_err() {
      return Ok(None);
    }

    Ok(Some((id, claimed)))
  }

  pub(super) fn publish_chunk(&self, id: &str, stream: &str, sequence: usize, data: &[u8]) -> Result<(), PathError> {
    let name = format!("{id}.{stream}.{sequence:0width$}", width = SEQUENCE_WIDTH);
    publish(&self.responses(), &name, data)
  }

  pub(super) fn write_refusal(&self, id: &str, message: &str) -> Result<(), PathError> {
    self.publish_chunk(id, ERROR_STREAM, 1, format!("compostbin: {message}\n").as_bytes())
  }

  pub(super) fn write_status(&self, id: &str, status: i32) -> Result<(), PathError> {
    publish(
      &self.responses(),
      &format!("{id}{STATUS_SUFFIX}"),
      format!("{status}\n"),
    )
  }

  /// Ids are timestamp-prefixed, so name order is arrival order.
  fn next_request_id(&self) -> Result<Option<String>, PathError> {
    let directory = self.requests();
    let mut oldest: Option<String> = None;

    for entry in std::fs::read_dir(&directory).at(&directory)? {
      let name = entry.at(&directory)?.file_name();

      if let Some(id) = name.to_string_lossy().strip_suffix(REQUEST_SUFFIX)
        && oldest.as_deref().is_none_or(|oldest| id < oldest)
      {
        oldest = Some(id.to_string());
      }
    }

    Ok(oldest)
  }
}

/// Written as `.partial` then renamed, so a reader across the mount never sees
/// it incomplete.
pub(super) fn publish(directory: &Path, name: &str, contents: impl AsRef<[u8]>) -> Result<(), PathError> {
  let partial = directory.join(format!("{name}{PARTIAL_SUFFIX}"));

  std::fs::write(&partial, contents).at(&partial)?;
  std::fs::rename(&partial, directory.join(name)).at(&partial)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::OUTPUT_STREAM;
  use crate::host::fixtures::spool;
  use crate::host::request::Request;
  use tempfile::TempDir;

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
