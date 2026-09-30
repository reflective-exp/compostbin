//! The manifest and session shared by the submodule tests.

use crate::session::Session;
use crate::workspace::paths::PathResolver;

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

/// Where `MANIFEST`'s project sits under a temp home: inside `~/workspace`,
/// so it is covered by that root.
pub const PROJECT: &str = "workspace/compostbin";

/// `MANIFEST` under a home that does not exist, for the tests that only assert
/// on paths.
pub fn session() -> Session {
  Session::new(
    toml::from_str(MANIFEST).expect("manifest should parse"),
    PathResolver::new("/Users/user/workspace/compostbin", "/Users/user"),
    "/Users/user/workspace/compostbin",
  )
}
