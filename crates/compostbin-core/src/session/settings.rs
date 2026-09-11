//! The host's own Claude, copied into the session's Claude home.
//!
//! Not the token — that is `credentials` — and not history or projects, which
//! belong to the session. Only what the user maintains once, on the host, and
//! expects to find in every container.

use crate::error::PathError;
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// The host user's own Claude home, the source of everything copied in here.
pub const HOST_CLAUDE_HOME: &str = "~/.claude";

/// What every session gets: the user's house style, their settings, their
/// skills. Not configurable and never named in a manifest, because this is the
/// user's Claude rather than a per-project decision.
pub const HOST_CLAUDE_SETTINGS: [&str; 3] = ["CLAUDE.md", "settings.json", "skills"];

/// Deep enough for any skill tree, shallow enough that a symlink loop under
/// `~/.claude` ends the copy instead of filling the disk.
const MAX_DEPTH: usize = 32;

/// Copies `HOST_CLAUDE_SETTINGS`, plus whatever `extra` the manifest adds, from
/// the host's `~/.claude` into the session's home, overwriting what is there. An
/// entry may be a file (`CLAUDE.md`) or a directory (`skills`), copied whole.
///
/// The host is authoritative: an edit there must reach the next session rather
/// than being shadowed by a stale copy, which is why a shared directory is
/// replaced rather than merged — a skill deleted on the host disappears from the
/// session too. Everything else in the session home — history, projects, the
/// refreshed token — is session state and is never overwritten from here. Names
/// are joined as single components, so a manifest cannot escape with `../`.
pub fn share(host_home: &Path, session_home: &Path, extra: &[String]) -> Result<Vec<String>, PathError> {
  let mut names: Vec<String> = HOST_CLAUDE_SETTINGS
    .iter()
    .map(|name| name.to_string())
    .collect();
  for name in extra {
    if !names.contains(name) {
      names.push(name.clone());
    }
  }

  let mut copied = Vec::new();
  let mut opened: Option<OwnedFd> = None;

  for name in names {
    if name.contains('/') || name == "." || name == ".." {
      continue;
    }

    let source = host_home.join(&name);
    // Metadata rather than the entry's own type: symlinking the skills directory
    // is a normal way to keep it under version control elsewhere.
    let Ok(metadata) = std::fs::metadata(&source) else {
      continue;
    };

    if !metadata.is_dir() && !metadata.is_file() {
      continue;
    }

    if opened.is_none() {
      std::fs::create_dir_all(session_home).map_err(|error| PathError::new(session_home, error))?;
      opened = Some(open_directory(session_home).map_err(|error| PathError::new(session_home, error))?);
    }
    let home = opened.as_ref().expect("opened above");
    let destination = session_home.join(&name);

    // The guest can write here while this runs, and a symlink it plants would
    // carry a path-based copy out onto the host. So everything below is created
    // through `home`'s descriptor, exclusively and without following links: a
    // link planted at any moment ends the copy with an error instead.
    remove(&destination)?;

    if metadata.is_dir() {
      copy_tree(&source, home, OsStr::new(&name), &destination, MAX_DEPTH)?;
    } else {
      copy_file(&source, &metadata, home, OsStr::new(&name), &destination)?;
    }

    copied.push(name);
  }

  Ok(copied)
}

/// Removes whatever is at `path`, directory or file; a no-op when nothing is.
fn remove(path: &Path) -> Result<(), PathError> {
  let Ok(metadata) = std::fs::symlink_metadata(path) else {
    return Ok(());
  };

  let removed = if metadata.is_dir() {
    std::fs::remove_dir_all(path)
  } else {
    std::fs::remove_file(path)
  };

  removed.map_err(|error| PathError::new(path, error))
}

/// Creates `name` inside `parent`. `destination` only names it in errors: a path
/// is resolved afresh on every call, and the guest can rearrange it in between.
fn copy_tree(source: &Path, parent: &OwnedFd, name: &OsStr, destination: &Path, depth: usize) -> Result<(), PathError> {
  if depth == 0 {
    return Ok(());
  }

  let directory = make_directory(parent, name).map_err(|error| PathError::new(destination, error))?;

  let entries = std::fs::read_dir(source).map_err(|error| PathError::new(source, error))?;
  for entry in entries {
    let entry = entry.map_err(|error| PathError::new(source, error))?;
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
  let mut from = File::open(source).map_err(|error| PathError::new(source, error))?;
  let mut to =
    create_file(parent, name, metadata.permissions().mode()).map_err(|error| PathError::new(destination, error))?;
  io::copy(&mut from, &mut to).map_err(|error| PathError::new(destination, error))?;
  Ok(())
}

/// The session home itself, refused if it is a link. Everything `share` writes
/// is created relative to this descriptor.
fn open_directory(path: &Path) -> io::Result<OwnedFd> {
  let file = OpenOptions::new()
    .read(true)
    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
    .open(path)?;
  Ok(file.into())
}

/// Creates `name` in `parent` and opens it. Fails, rather than following, when
/// something is already there or is swapped for a link between the two calls.
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

/// A new file in `parent`. `O_EXCL` refuses whatever is already at `name`, a
/// link included, so nothing is ever written through one.
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

  /// A host home holding every settings entry, so a test can assert on what a
  /// case adds rather than on the baseline.
  fn host_home(temp: &TempDir) -> std::path::PathBuf {
    let host = temp.path().join("host-claude");
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
    std::fs::write(host.join("agents.json"), "[]").expect("write agents");

    let copied =
      share(&host, &session, &["agents.json".to_string(), "CLAUDE.md".to_string()]).expect("share should succeed");

    assert_eq!(copied, ["CLAUDE.md", "settings.json", "skills", "agents.json"]);
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

  /// What a guest racing `share` would do: plant a link after `remove` has
  /// cleared the name.
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
