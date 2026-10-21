use crate::error::{At, PathError};
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const HOST_CLAUDE_SETTINGS: [&str; 4] = ["CLAUDE.md", "agents", "settings.json", "skills"];

/// Bounds the copy so a symlink loop under `~/.claude` terminates.
const MAX_DEPTH: usize = 32;

/// Replaces each entry wholesale rather than merging, so deletions on the host propagate.
pub fn share(host_home: &Path, session_home: &Path, extra: &[String]) -> Result<Vec<String>, PathError> {
  let mut names: Vec<String> = HOST_CLAUDE_SETTINGS
    .iter()
    .copied()
    .map(str::to_string)
    .collect();
  for name in extra {
    if !names.contains(name) {
      names.push(name.clone());
    }
  }

  let mut copied = Vec::new();
  let mut opened: Option<OwnedFd> = None;

  for name in names {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
      continue;
    }

    let source = host_home.join(&name);
    // Follows links, since a symlinked skills directory is common.
    let Ok(metadata) = std::fs::metadata(&source) else {
      continue;
    };

    if !metadata.is_dir() && !metadata.is_file() {
      continue;
    }

    let home = match &opened {
      Some(home) => home,
      None => {
        std::fs::create_dir_all(session_home).at(session_home)?;
        opened.insert(open_directory(session_home).at(session_home)?)
      }
    };
    let destination = session_home.join(&name);

    // The guest can plant symlinks here concurrently, so everything is created through `home`'s
    // descriptor without following links.
    crate::fs::remove(&destination)?;

    if metadata.is_dir() {
      copy_tree(&source, home, OsStr::new(&name), &destination, MAX_DEPTH)?;
    } else {
      copy_file(&source, &metadata, home, OsStr::new(&name), &destination)?;
    }

    copied.push(name);
  }

  Ok(copied)
}

/// `destination` is only for error messages, since the guest can rearrange the path between calls.
fn copy_tree(source: &Path, parent: &OwnedFd, name: &OsStr, destination: &Path, depth: usize) -> Result<(), PathError> {
  if depth == 0 {
    return Ok(());
  }

  let directory = make_directory(parent, name).at(destination)?;

  for entry in std::fs::read_dir(source).at(source)? {
    let entry = entry.at(source)?;
    let child = entry.path();
    let name = entry.file_name();
    let target = destination.join(&name);

    let Ok(metadata) = std::fs::metadata(&child) else {
      continue;
    };

    if metadata.is_dir() {
      copy_tree(&child, &directory, &name, &target, depth - 1)?;
    } else if metadata.is_file() {
      copy_file(&child, &metadata, &directory, &name, &target)?;
    }
  }

  Ok(())
}

fn copy_file(
  source: &Path,
  metadata: &Metadata,
  parent: &OwnedFd,
  name: &OsStr,
  destination: &Path,
) -> Result<(), PathError> {
  let mut from = File::open(source).at(source)?;
  let mut to = create_file(parent, name, metadata.permissions().mode()).at(destination)?;
  io::copy(&mut from, &mut to).at(destination)?;
  Ok(())
}

fn open_directory(path: &Path) -> io::Result<OwnedFd> {
  let file = OpenOptions::new()
    .read(true)
    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
    .open(path)?;
  Ok(file.into())
}

fn make_directory(parent: &OwnedFd, name: &OsStr) -> io::Result<OwnedFd> {
  let name = c_name(name)?;

  // SAFETY: `parent` is an open descriptor and `name` is NUL-terminated.
  if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) } < 0 {
    return Err(io::Error::last_os_error());
  }

  // SAFETY: as above.
  let opened = unsafe {
    libc::openat(
      parent.as_raw_fd(),
      name.as_ptr(),
      libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    )
  };

  if opened < 0 {
    return Err(io::Error::last_os_error());
  }

  // SAFETY: freshly opened and owned by us alone.
  Ok(unsafe { OwnedFd::from_raw_fd(opened) })
}

fn create_file(parent: &OwnedFd, name: &OsStr, mode: u32) -> io::Result<File> {
  let name = c_name(name)?;

  // SAFETY: `parent` is an open descriptor and `name` is NUL-terminated.
  let opened = unsafe {
    libc::openat(
      parent.as_raw_fd(),
      name.as_ptr(),
      libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
      (mode & 0o7777) as libc::c_uint,
    )
  };

  if opened < 0 {
    return Err(io::Error::last_os_error());
  }

  // SAFETY: freshly opened and owned by us alone.
  Ok(File::from(unsafe { OwnedFd::from_raw_fd(opened) }))
}

fn c_name(name: &OsStr) -> io::Result<CString> {
  CString::new(name.as_bytes()).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn host_home(temp: &TempDir) -> std::path::PathBuf {
    let host = temp.path().join("host-claude");
    std::fs::create_dir_all(host.join("agents")).expect("create host agents");
    std::fs::create_dir_all(host.join("skills")).expect("create host skills");
    std::fs::write(host.join("CLAUDE.md"), "house style").expect("write CLAUDE.md");
    std::fs::write(host.join("settings.json"), "{}").expect("write settings");
    host
  }

  #[test]
  fn copies_the_host_settings_without_being_asked() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::write(host.join("history.jsonl"), "not shared").expect("write history");

    let copied = share(&host, &session, &[]).expect("share should succeed");

    assert_eq!(copied, HOST_CLAUDE_SETTINGS);
    assert_eq!(
      std::fs::read_to_string(session.join("CLAUDE.md")).expect("CLAUDE.md should be copied"),
      "house style"
    );
    assert!(!session.join("history.jsonl").exists(), "only the settings are shared");
  }

  #[test]
  fn adds_what_the_manifest_asks_for_and_names_it_once() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(host.join("widgets")).expect("create widgets");

    let copied =
      share(&host, &session, &["widgets".to_string(), "CLAUDE.md".to_string()]).expect("share should succeed");

    assert_eq!(copied, ["CLAUDE.md", "agents", "settings.json", "skills", "widgets"]);
  }

  #[test]
  fn copies_a_linked_agent_as_its_contents() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create elsewhere");
    std::fs::write(elsewhere.join("reviewer.md"), "an agent").expect("write agent");
    std::os::unix::fs::symlink(elsewhere.join("reviewer.md"), host.join("agents").join("reviewer.md"))
      .expect("link agent");

    share(&host, &session, &[]).expect("share should succeed");

    let copied = session.join("agents").join("reviewer.md");
    assert!(
      !std::fs::symlink_metadata(&copied)
        .expect("agent should be copied")
        .file_type()
        .is_symlink()
    );
    assert_eq!(
      std::fs::read_to_string(&copied).expect("agent should be readable"),
      "an agent"
    );
  }

  #[test]
  fn follows_the_host_on_every_run() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::write(session.join("CLAUDE.md"), "stale").expect("write stale copy");
    std::fs::write(host.join("CLAUDE.md"), "edited on the host").expect("write CLAUDE.md");

    share(&host, &session, &[]).expect("share should succeed");

    assert_eq!(
      std::fs::read_to_string(session.join("CLAUDE.md")).expect("CLAUDE.md should exist"),
      "edited on the host"
    );
  }

  #[test]
  fn replaces_a_planted_symlink() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    let victim = temp.path().join("zshrc");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::write(&victim, "the host's own file").expect("write victim");
    std::os::unix::fs::symlink(&victim, session.join("CLAUDE.md")).expect("plant symlink");

    share(&host, &session, &[]).expect("share should succeed");

    assert_eq!(
      std::fs::read_to_string(&victim).expect("victim should exist"),
      "the host's own file",
      "a copy must never land outside the session home"
    );
    assert!(
      !std::fs::symlink_metadata(session.join("CLAUDE.md"))
        .expect("CLAUDE.md should exist")
        .file_type()
        .is_symlink()
    );
    assert_eq!(
      std::fs::read_to_string(session.join("CLAUDE.md")).expect("CLAUDE.md should exist"),
      "house style"
    );
  }

  #[test]
  fn refuses_a_file_link_planted_mid_copy() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    let victim = temp.path().join("zshrc");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::write(&victim, "the host's own file").expect("write victim");
    std::os::unix::fs::symlink(&victim, session.join("CLAUDE.md")).expect("plant symlink");
    let home = open_directory(&session).expect("open session home");
    let source = host.join("CLAUDE.md");
    let metadata = std::fs::metadata(&source).expect("metadata");

    let copied = copy_file(
      &source,
      &metadata,
      &home,
      OsStr::new("CLAUDE.md"),
      &session.join("CLAUDE.md"),
    );

    assert!(copied.is_err(), "a link in the way must stop the copy");
    assert_eq!(
      std::fs::read_to_string(&victim).expect("victim should exist"),
      "the host's own file"
    );
  }

  #[test]
  fn refuses_a_directory_link_planted_mid_copy() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    let victim = temp.path().join("elsewhere");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::create_dir_all(&victim).expect("create victim");
    std::fs::write(host.join("skills").join("SKILL.md"), "how to deploy").expect("write skill");
    std::os::unix::fs::symlink(&victim, session.join("skills")).expect("plant symlink");
    let home = open_directory(&session).expect("open session home");

    let copied = copy_tree(
      &host.join("skills"),
      &home,
      OsStr::new("skills"),
      &session.join("skills"),
      MAX_DEPTH,
    );

    assert!(copied.is_err(), "a link in the way must stop the copy");
    assert!(
      std::fs::read_dir(&victim)
        .expect("victim should exist")
        .next()
        .is_none(),
      "nothing may land where the link points"
    );
  }

  /// An empty name would make `remove` wipe the whole session home.
  #[test]
  fn ignores_an_empty_name() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&session).expect("create session home");
    std::fs::write(session.join("history.jsonl"), "session state").expect("write history");

    let copied = share(&host, &session, &[String::new()]).expect("share should succeed");

    assert_eq!(copied, HOST_CLAUDE_SETTINGS);
    assert_eq!(
      std::fs::read_to_string(session.join("history.jsonl")).expect("history should survive"),
      "session state"
    );
  }

  #[test]
  fn copies_a_shared_directory_whole() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(host.join("skills").join("deploy")).expect("create skill");
    std::fs::write(host.join("skills").join("deploy").join("SKILL.md"), "how to deploy").expect("write skill");

    share(&host, &session, &[]).expect("share should succeed");

    assert_eq!(
      std::fs::read_to_string(session.join("skills").join("deploy").join("SKILL.md")).expect("skill should be copied"),
      "how to deploy"
    );
  }

  #[test]
  fn drops_a_skill_the_host_has_deleted() {
    let temp = TempDir::new().expect("temp dir");
    let host = host_home(&temp);
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(session.join("skills").join("retired")).expect("create stale skill");
    std::fs::write(session.join("skills").join("retired").join("SKILL.md"), "gone").expect("write stale skill");

    share(&host, &session, &[]).expect("share should succeed");

    assert!(
      !session.join("skills").join("retired").exists(),
      "the host is authoritative: a deleted skill must not linger"
    );
  }

  #[test]
  fn shares_nothing_the_host_does_not_have() {
    let temp = TempDir::new().expect("temp dir");
    let host = temp.path().join("host-claude");
    let session = temp.path().join("session-claude");
    std::fs::create_dir_all(&host).expect("create host home");

    let copied = share(&host, &session, &["../.ssh/id_ed25519".to_string()]).expect("share should succeed");

    assert_eq!(copied, Vec::<String>::new());
    assert!(!session.exists(), "nothing to copy means nothing to create");
  }
}
