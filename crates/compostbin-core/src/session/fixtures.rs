use crate::session::Session;
use crate::workspace::paths::PathResolver;
use std::path::Path;
use tempfile::TempDir;

pub const MANIFEST: &str = r#"
[project]
name = "cb"

[container]
cpus   = 4
env    = ["ANTHROPIC_API_KEY"]
memory = "8G"

[workspace]
roots = ["~/workspace"]

[[paths]]
readonly = true
source   = "~/.cargo/registry"
"#;

pub const PROJECT: &str = "workspace/compostbin";

/// The home does not exist, so this suits only tests that assert on paths.
pub fn session() -> Session {
  session_at(Path::new("/Users/user"), MANIFEST, PROJECT)
}

pub fn temp_session(manifest: &str, project: &str) -> (TempDir, Session) {
  let temp = TempDir::new().expect("temp dir");
  let session = session_at(&temp.path().canonicalize().expect("canonical temp"), manifest, project);
  (temp, session)
}

/// A real `base` must be canonical, since macOS's `/var` is a symlink to `/private/var`.
pub fn session_at(base: &Path, manifest: &str, project: &str) -> Session {
  Session::new(
    toml::from_str(manifest).expect("manifest should parse"),
    PathResolver::new(base.join(project), base),
    base.join(project),
  )
}
