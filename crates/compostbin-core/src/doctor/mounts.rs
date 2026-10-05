//! What can be wrong about what the container can see: a declared path that is
//! not there, a link that dies at the mount boundary, a mount set the running
//! container never got, and a mount that hands over too much.

use super::{Check, Status, check, listed};
use crate::manifest::TomlFile;
use crate::session::Session;
use crate::session::record::Record;
use crate::workspace::danger::danger;
use crate::workspace::{Escape, Origin};

/// Directory entries `escaping_symlinks` looks at before giving up: enough for
/// an ordinary source tree, few enough to stay imperceptible.
const WALK_LIMIT: usize = 50_000;

/// Roots and explicit paths only. The project directory is where compostbin
/// was invoked, so it exists; Claude's home is excluded: `run` creates it, so
/// it is normally absent before the first session.
pub fn mounted_paths(session: &Session) -> Check {
  let missing: Vec<String> = session
    .workspace()
    .entries()
    .iter()
    .filter(|entry| entry.origin != Origin::Project && !entry.host.exists())
    .map(|entry| entry.host.display().to_string())
    .collect();

  if missing.is_empty() {
    return check("mounted paths", Status::Ok, "every declared path exists");
  }

  listed(
    "mounted paths",
    Status::Fail,
    "declared, but missing on the host",
    missing,
  )
}

/// Symlinks that resolve on the host but dangle in the container, their targets
/// outside every mounted tree.
///
/// A warning: the session runs, and whether to mount the target or drop the
/// link is the user's call.
pub fn dangling_symlinks(session: &Session) -> Check {
  const NAMED: usize = 5;

  let found = session.workspace().escaping_symlinks(WALK_LIMIT);

  if found.escapes.is_empty() {
    let detail = if found.exhausted {
      format!("none in the first {WALK_LIMIT} entries; the mounted trees are too large to walk in full")
    } else {
      "no symlink escapes the mounted trees".to_string()
    };

    return check("dangling symlinks", Status::Ok, detail);
  }

  let mut named: Vec<String> = found
    .escapes
    .iter()
    .take(NAMED)
    .map(Escape::to_string)
    .collect();

  // In the list, so the sentence stays true however many were named.
  let rest = found.escapes.len().saturating_sub(named.len());
  if rest > 0 {
    named.push(format!("and {rest} more"));
  }

  listed(
    "dangling symlinks",
    Status::Warn,
    "dead in the container, because the target is not mounted",
    named,
  )
}

/// Whether the running container has the mounts the manifest describes. `run`
/// attaches to a live container, and mounts cannot be added to one, so a
/// manifest edit takes effect only once `run` recreates it. Otherwise the path
/// is just missing in the guest.
///
/// A warning, since recreating is the user's call, but the fix is named because
/// it is not guessable.
pub fn live_mounts(session: &Session, running: bool) -> Check {
  let name = session.container_name();

  if !running {
    return check(
      "container mounts",
      Status::Ok,
      format!("{name} is not running, so the next `compostbin run` mounts what the manifest says"),
    );
  }

  let record = match Record::load_if_present(&session.mount_record()) {
    Ok(Some(record)) => record,
    Ok(None) => {
      return check(
        "container mounts",
        Status::Warn,
        format!(
          "{name} is running but nothing recorded what it was started with; exit it and `compostbin run` again to be sure"
        ),
      );
    }
    Err(error) => return check("container mounts", Status::Warn, error.to_string()),
  };

  let drift = record.drift(&session.mounts(), &session.sockets());

  if drift.is_empty() {
    return check(
      "container mounts",
      Status::Ok,
      format!("{name} is running with the mounts the manifest declares"),
    );
  }

  listed(
    "container mounts",
    Status::Warn,
    format!(
      "{name} was started before the manifest changed; mounts cannot be added to a running container, so exit it and `compostbin run` again"
    ),
    drift.lines(),
  )
}

/// Every mounted path, not only roots: a hand-edited manifest bypasses `add`'s
/// refusal.
pub fn root_breadth(session: &Session) -> Check {
  let dangerous: Vec<String> = session
    .workspace()
    .entries()
    .iter()
    .filter_map(|entry| {
      // Judged as `add` judges it: `~/code/../.ssh`, or a link to `~/.ssh`, is
      // still `~/.ssh`. A missing path is judged as written.
      let judged = entry
        .host
        .canonicalize()
        .unwrap_or_else(|_| entry.host.clone());
      danger(&judged, session.resolver()).map(|danger| format!("{}: {danger}", entry.host.display()))
    })
    .collect();

  if dangerous.is_empty() {
    return check("mount breadth", Status::Ok, "no mount covers an account or a secret");
  }

  listed(
    "mount breadth",
    Status::Warn,
    "a mount reaches past the project",
    dangerous,
  )
}
