use crate::error::{At, PathError};
use crate::fs::{empty, remove};
use crate::host::Spool;
use crate::session::Session;
use crate::session::briefing::MANAGED_SETTINGS_DIR;
use crate::session::record::RECORD_FILE;
use std::path::{Path, PathBuf};

const SESSIONS_DIR: &str = "~/.local/state/compostbin/sessions";
const CLAUDE_HOME_DIR: &str = "claude-home";
const SPOOL_DIR: &str = "host";
const PORTS_LOG: &str = "ports.log";

/// Empties directories instead of removing them, because a running container's mount stays bound to the original inode.
/// Symlinks are removed, not followed.
fn clear(target: &Path) -> Result<(), PathError> {
  if std::fs::symlink_metadata(target).at(target)?.is_dir() {
    empty(target)
  } else {
    remove(target)
  }
}

impl Session {
  pub fn claude_home(&self) -> PathBuf {
    match &self.manifest.claude.home {
      Some(home) => self.resolver.resolve(home),
      None => self.state_dir().join(CLAUDE_HOME_DIR),
    }
  }

  pub fn sessions_dir(&self) -> PathBuf {
    self.resolver.resolve(SESSIONS_DIR)
  }

  pub fn state_dir(&self) -> PathBuf {
    self.sessions_dir().join(self.container_name())
  }

  fn transient_state(&self) -> Vec<PathBuf> {
    vec![self.host_spool(), self.ports_log(), self.managed_settings()]
  }

  /// Without `everything`, empties paths in place because the container may still mount them.
  pub fn clean(&self, everything: bool) -> Result<Vec<PathBuf>, PathError> {
    if everything {
      let state = self.state_dir();
      if !state.exists() {
        return Ok(Vec::new());
      }
      std::fs::remove_dir_all(&state).at(&state)?;
      return Ok(vec![state]);
    }

    let spool = self.host_spool();
    let mut cleared = Vec::new();

    for target in self.transient_state() {
      if !target.exists() {
        continue;
      }

      if target == spool {
        Spool::new(&target).empty()?;
      } else {
        clear(&target)?;
      }

      cleared.push(target);
    }

    Ok(cleared)
  }

  pub(super) fn clean_after_exit(&self) -> Result<(), PathError> {
    let spool = self.host_spool();
    if !spool.exists() {
      return Ok(());
    }

    Spool::new(&spool).empty()
  }

  pub fn host_spool(&self) -> PathBuf {
    self.state_dir().join(SPOOL_DIR)
  }

  pub(super) fn spool(&self) -> Option<Spool> {
    self
      .manifest
      .host
      .has_commands()
      .then(|| Spool::new(self.host_spool()))
  }

  pub(super) fn prepare_host_spool(&self) -> Result<(), PathError> {
    match self.spool() {
      Some(spool) => spool.create(),
      None => Ok(()),
    }
  }

  pub fn ports_log(&self) -> PathBuf {
    self.state_dir().join(PORTS_LOG)
  }

  pub fn managed_settings(&self) -> PathBuf {
    self.state_dir().join(MANAGED_SETTINGS_DIR)
  }

  pub fn mount_record(&self) -> PathBuf {
    self.state_dir().join(RECORD_FILE)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::fixtures::{self, MANIFEST, PROJECT, session};
  use std::os::unix::fs::MetadataExt;

  #[test]
  fn resolves_claude_home_on_the_host() {
    assert_eq!(
      session().claude_home(),
      PathBuf::from("/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home")
    );
  }

  #[test]
  fn prepares_the_spool_only_when_commands_are_declared() {
    let (_temp, mut session) = fixtures::temp_session(MANIFEST, PROJECT);

    session
      .prepare_host_spool()
      .expect("prepare should succeed");
    assert!(!session.host_spool().exists(), "no allowlist, no spool");

    session.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");
    session
      .prepare_host_spool()
      .expect("prepare should succeed");
    assert!(session.host_spool().exists());
  }

  #[test]
  fn cleans_transient_state_but_keeps_the_conversation() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");
    std::fs::write(session.host_spool().join("requests").join("0001.request"), "run\n").expect("write a request");
    std::fs::create_dir_all(session.managed_settings()).expect("create managed settings");
    std::fs::write(session.managed_settings().join("session-context.txt"), "briefing").expect("write a briefing");
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    let cleared = session.clean(false).expect("clean should succeed");

    assert_eq!(cleared, [session.host_spool(), session.managed_settings()]);
    assert!(
      session.host_spool().join("requests").exists(),
      "the container may still hold this mount, and a directory it lost cannot be given back"
    );
    assert_eq!(
      std::fs::read_dir(session.host_spool().join("requests"))
        .expect("read requests")
        .count(),
      0,
      "what was in flight is gone"
    );
    assert!(
      session.managed_settings().exists(),
      "the same mount argument: the briefing is rewritten on the next create"
    );
    assert_eq!(
      std::fs::read_dir(session.managed_settings())
        .expect("read managed settings")
        .count(),
      0,
      "keeping a stale briefing would be worse than an empty directory"
    );
    assert!(
      session.claude_home().exists(),
      "the conversation `--continue` reattaches to must survive"
    );
  }

  #[test]
  fn cleaning_keeps_the_inode_a_running_container_mounts() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");

    let before = std::fs::metadata(session.host_spool())
      .expect("the spool")
      .ino();
    session.clean(false).expect("clean should succeed");
    session
      .clean_after_exit()
      .expect("exit cleanup should succeed");
    let after = std::fs::metadata(session.host_spool())
      .expect("the spool")
      .ino();

    assert_eq!(before, after, "the mount source must be the same directory throughout");
  }

  #[test]
  fn exit_cleanup_keeps_managed_settings() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");
    std::fs::write(session.host_spool().join("running").join("0001"), "claimed").expect("write a claim");
    std::fs::create_dir_all(session.managed_settings()).expect("create managed settings");

    session.clean_after_exit().expect("clean should succeed");

    assert_eq!(
      std::fs::read_dir(session.host_spool().join("running"))
        .expect("read running")
        .count(),
      0,
      "nothing claimed can outlive the session that claimed it"
    );
    assert!(
      session.host_spool().join("requests").exists(),
      "the container is still holding this mount"
    );
    assert!(
      session.managed_settings().exists(),
      "the container outlives the exit, and the next `run` attaches without rewriting the briefing"
    );
  }

  #[test]
  fn cleaning_everything_takes_the_session_directory() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      [session.state_dir()]
    );
    assert!(!session.state_dir().exists());
  }

  #[test]
  fn cleaning_state_that_is_already_gone_is_not_an_error() {
    let (_temp, session) = fixtures::temp_session(MANIFEST, PROJECT);

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      Vec::<PathBuf>::new()
    );
  }
}
