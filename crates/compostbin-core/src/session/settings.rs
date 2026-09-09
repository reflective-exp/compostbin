//! The host's own Claude, copied into the session's Claude home.
//!
//! Not the token — that is `credentials` — and not history or projects, which
//! belong to the session. Only what the user maintains once, on the host, and
//! expects to find in every container.

use crate::error::PathError;
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

    std::fs::create_dir_all(session_home).map_err(|error| PathError::new(session_home, error))?;
    let destination = session_home.join(&name);

    if metadata.is_dir() {
      remove(&destination)?;
      copy_tree(&source, &destination, MAX_DEPTH)?;
    } else if metadata.is_file() {
      std::fs::copy(&source, &destination).map_err(|error| PathError::new(&destination, error))?;
    } else {
      continue;
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

fn copy_tree(source: &Path, destination: &Path, depth: usize) -> Result<(), PathError> {
  if depth == 0 {
    return Ok(());
  }

  std::fs::create_dir_all(destination).map_err(|error| PathError::new(destination, error))?;

  let entries = std::fs::read_dir(source).map_err(|error| PathError::new(source, error))?;
  for entry in entries {
    let entry = entry.map_err(|error| PathError::new(source, error))?;
    let child = entry.path();
    let target = destination.join(entry.file_name());

    let Ok(metadata) = std::fs::metadata(&child) else {
      continue;
    };

    if metadata.is_dir() {
      copy_tree(&child, &target, depth - 1)?;
    } else if metadata.is_file() {
      std::fs::copy(&child, &target).map_err(|error| PathError::new(&target, error))?;
    }
  }

  Ok(())
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
