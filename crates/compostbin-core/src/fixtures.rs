//! Session fixtures shared by the module tests.
//!
//! Anything that creates a container, builds an image or cleans state writes
//! under the session's home, so those tests need a home that really exists
//! rather than the string paths the pure ones assert on.

use crate::session::Session;
use crate::workspace::paths::PathResolver;
use std::path::Path;
use tempfile::TempDir;

/// A session under a fresh temp home, built from `manifest` with `project` as
/// its directory relative to that home. An empty manifest is the default one.
///
/// The `TempDir` comes back because dropping it takes the home with it.
pub fn session(manifest: &str, project: &str) -> (TempDir, Session) {
  session_in(TempDir::new().expect("temp dir"), manifest, project)
}

/// The same, in a temp directory the caller chose — `/tmp` for the tests whose
/// sockets must stay inside the length a socket path may have.
pub fn session_in(temp: TempDir, manifest: &str, project: &str) -> (TempDir, Session) {
  let session = session_at(&temp.path().canonicalize().expect("canonical temp"), manifest, project);
  (temp, session)
}

/// For a test that has to know the home before it can write the manifest naming
/// it. `base` must already be canonical: on macOS `/var` is a symlink to
/// `/private/var`, which would make every path in an assertion the other one.
pub fn session_at(base: &Path, manifest: &str, project: &str) -> Session {
  Session::new(
    toml::from_str(manifest).expect("manifest should parse"),
    PathResolver::new(base.join(project), base),
    base.join(project),
  )
}
