//! What a session keeps on the host, and how much of it `clean` may take.

use crate::error::{At, PathError};
use crate::host::Spool;
use crate::manifest::SESSIONS_DIR;
use crate::session::Session;
use crate::session::briefing::MANAGED_SETTINGS_DIR;
use crate::session::record::RECORD_FILE;
use std::path::{Path, PathBuf};

/// Under the session state directory: kept, because `claude --continue` reads it.
pub const CLAUDE_HOME_DIR: &str = "claude-home";
/// Under the session state directory: requests in flight. `clean` empties it
/// rather than removing it, because the container mounts it.
pub const SPOOL_DIR: &str = "host";
/// Under the session state directory: one bound socket per declared port,
/// owned by the `run` that created the container and living exactly as long.
pub const PORTS_DIR: &str = "ports";
/// Where the relay says what it could not, while a session is attached.
pub const PORTS_LOG: &str = "ports.log";

/// Empties a directory without unlinking it, or removes a plain file.
///
/// Every transient path a session keeps is also a mount source, and a running
/// container's mount is attached to the inode it was created with: a directory
/// removed and recreated at the same path is never reattached, so the guest is
/// left holding one that nothing writes to any more. Emptying is the only kind
/// of cleaning a live mount survives.
///
/// A symlink is removed rather than followed, so a link planted where a mount
/// source belongs cannot make this clear something else.
fn clear(target: &Path) -> Result<(), PathError> {
  let kind = std::fs::symlink_metadata(target).at(target)?.file_type();

  if !kind.is_dir() {
    return std::fs::remove_file(target).at(target);
  }

  for entry in std::fs::read_dir(target).at(target)? {
    let entry = entry.at(target)?;
    let path = entry.path();

    let outcome = if entry.file_type().at(&path)?.is_dir() {
      std::fs::remove_dir_all(&path)
    } else {
      std::fs::remove_file(&path)
    };

    outcome.at(&path)?;
  }

  Ok(())
}

impl Session {
  /// The source side of the bind mount, and so where credentials are seeded
  /// before the container starts.
  ///
  /// Per session unless the manifest overrides it: one shared home would make
  /// `--continue` resume whichever project ran last, and expose one project's
  /// history to another's container.
  pub fn claude_home(&self) -> PathBuf {
    match &self.manifest.claude.home {
      Some(home) => self.resolver.resolve(home),
      None => self.state_dir().join(CLAUDE_HOME_DIR),
    }
  }

  /// Everything this session keeps on the host, under its own container name.
  pub fn state_dir(&self) -> PathBuf {
    self
      .resolver
      .resolve(SESSIONS_DIR)
      .join(self.container_name())
  }

  /// What `clean` may delete: state meaningless once the container is gone. Not
  /// Claude's home, which holds the conversation `--continue` reattaches to.
  fn transient_state(&self) -> Vec<PathBuf> {
    vec![
      self.host_spool(),
      self.port_sockets(),
      self.ports_log(),
      self.managed_settings(),
    ]
  }

  /// Empties this session's transient state, and with `everything` removes the
  /// session directory whole. Missing paths are not an error: `clean` exists
  /// because an earlier exit may not have run.
  ///
  /// Emptied rather than removed: the container may still be running, and each
  /// of these paths is a mount source (§`clear`). `everything` is the exception:
  /// it discards the conversation, so the session is over by definition.
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

  /// What the `run` that created the container clears when Claude exits: only
  /// the spool, so nothing claimed outlives the agent that claimed it.
  ///
  /// Emptied rather than removed: until this process exits the container still
  /// holds that mount (§`clear`). The rest is left for the next create, which
  /// rebinds the sockets and rewrites the managed settings anyway.
  pub(super) fn clean_after_exit(&self) -> Result<Vec<PathBuf>, PathError> {
    let spool = self.host_spool();
    if !spool.exists() {
      return Ok(Vec::new());
    }

    Spool::new(&spool).empty()?;

    Ok(vec![spool])
  }

  /// Inside the session directory, so concurrent projects cannot see each
  /// other's requests and `clean` takes it with the rest.
  pub fn host_spool(&self) -> PathBuf {
    self.state_dir().join(SPOOL_DIR)
  }

  /// The spool at `host_spool`, or `None` with no commands declared: with no
  /// allowlist there is no channel at all, rather than an empty one.
  pub(super) fn spool(&self) -> Option<Spool> {
    self
      .manifest
      .host
      .has_commands()
      .then(|| Spool::new(self.host_spool()))
  }

  /// The mount source must exist before the container starts, and the guest
  /// cannot create it. A no-op with no commands declared.
  pub fn prepare_host_spool(&self) -> Result<(), PathError> {
    match self.spool() {
      Some(spool) => spool.create(),
      None => Ok(()),
    }
  }

  /// Where this session's port sockets are bound. Inside the session directory
  /// because the directory is what confines them: socket mode cannot (the guest
  /// end is root-owned, the session is not).
  pub fn port_sockets(&self) -> PathBuf {
    self.state_dir().join(PORTS_DIR)
  }

  /// Where the relay writes once Claude is attached.
  ///
  /// A file, because by then the terminal is Claude's: the relay runs on a
  /// thread of `run`, and anything it printed would land mid-draw. A host
  /// service not yet started is ordinary (`[host.commands]` may start it), so a
  /// refused connection must not look like a fault.
  pub fn ports_log(&self) -> PathBuf {
    self.state_dir().join(PORTS_LOG)
  }

  /// The source side of the managed settings mount: what this session tells its
  /// Claude about itself. Per session, because it is rendered from this
  /// project's manifest.
  pub fn managed_settings(&self) -> PathBuf {
    self.state_dir().join(MANAGED_SETTINGS_DIR)
  }

  /// Where the container's creation-time mounts are recorded, so `doctor` can
  /// tell a manifest edited mid-session from one that took effect.
  pub fn mount_record(&self) -> PathBuf {
    self.state_dir().join(RECORD_FILE)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::fixtures;
  use crate::session::fixtures::{MANIFEST, PROJECT, session};
  use std::os::unix::fs::MetadataExt;

  #[test]
  fn resolves_claude_home_on_the_host() {
    assert_eq!(
      session().claude_home(),
      PathBuf::from("/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home")
    );
  }

  /// `prepare` creates the mount source, so an undeclared channel must leave
  /// nothing behind.
  #[test]
  fn prepares_the_spool_only_when_commands_are_declared() {
    let (_temp, mut session) = fixtures::session(MANIFEST, PROJECT);

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
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
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

  /// A spool unlinked while the container holds it leaves every later `shell`
  /// and `host-agent` writing to an inode the guest cannot reach: a host channel
  /// present and permanently empty.
  #[test]
  fn cleaning_keeps_the_inode_a_running_container_mounts() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
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
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    Spool::new(session.host_spool())
      .create()
      .expect("create spool");
    std::fs::write(session.host_spool().join("running").join("0001"), "claimed").expect("write a claim");
    std::fs::create_dir_all(session.managed_settings()).expect("create managed settings");

    let cleared = session.clean_after_exit().expect("clean should succeed");

    assert_eq!(cleared, [session.host_spool()]);
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
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);
    std::fs::create_dir_all(session.claude_home()).expect("create claude home");

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      [session.state_dir()]
    );
    assert!(!session.state_dir().exists());
  }

  #[test]
  fn cleaning_state_that_is_already_gone_is_not_an_error() {
    let (_temp, session) = fixtures::session(MANIFEST, PROJECT);

    assert_eq!(
      session.clean(true).expect("clean should succeed"),
      Vec::<PathBuf>::new()
    );
  }
}
