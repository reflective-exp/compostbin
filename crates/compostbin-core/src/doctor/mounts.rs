use super::{Check, Status, check, listed};
use crate::manifest::TomlFile;
use crate::session::Session;
use crate::session::record::Record;
use crate::workspace::danger::danger;
use crate::workspace::{Escape, Origin};

const WALK_LIMIT: usize = 50_000;

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

  let drift = record.drift(&session.mounts(), &session.manifest.host);

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

/// Checks every path, not only roots, because a hand-edited manifest bypasses
/// `add`'s refusal.
pub fn root_breadth(session: &Session) -> Check {
  let dangerous: Vec<String> = session
    .workspace()
    .entries()
    .iter()
    .filter_map(|entry| {
      // Canonicalized as `add` does, so `~/code/../.ssh` or a link to `~/.ssh` is still caught.
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
