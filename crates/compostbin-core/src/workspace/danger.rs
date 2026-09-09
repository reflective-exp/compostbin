//! Which host paths should probably not be mounted at all.
//!
//! Policy rather than plumbing: `paths` decides what a manifest string means,
//! this decides what to say about the answer. `add` refuses on either verdict
//! without `--force`; `doctor` reports both, a manifest being hand-editable.

use crate::workspace::paths::PathResolver;
use std::path::{Path, PathBuf};

/// Paths whose contents are secrets: mounting one, or anything above it, hands
/// the container the keys to accounts the session has no business touching.
pub const SENSITIVE_PATHS: [&str; 7] = [
  "/etc",
  "/private/etc",
  "~/.aws",
  "~/.gnupg",
  "~/.kube",
  "~/.ssh",
  "~/Library/Keychains",
];
/// Paths not secret in themselves, but covering a whole account or machine — so
/// mounting one mounts everything below it, the paths above included.
pub const BROAD_PATHS: [&str; 6] = ["/", "/Users", "~", "~/Desktop", "~/Documents", "~/Downloads"];

/// Why a path should probably not be mounted.
#[derive(Clone, Debug, PartialEq)]
pub enum Danger {
  /// Wide enough to expose the whole account, `SENSITIVE_PATHS` included.
  Broad(PathBuf),
  /// The path is, or lies inside, something that holds credentials.
  Sensitive(PathBuf),
}

impl std::fmt::Display for Danger {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Broad(path) => write!(
        formatter,
        "{} covers a whole account or machine, so mounting it mounts every secret under it",
        path.display()
      ),
      Self::Sensitive(path) => write!(formatter, "{} holds credentials", path.display()),
    }
  }
}

/// Judges an already-resolved host path. A short list of obvious mistakes rather
/// than a containment boundary: the container runs with the user's own
/// privileges either way, so this guards against a slip, not against the user.
pub fn danger(path: &Path, resolver: &PathResolver) -> Option<Danger> {
  if let Some(sensitive) = SENSITIVE_PATHS
    .iter()
    .map(|raw| resolver.resolve(raw))
    .find(|sensitive| path.starts_with(sensitive))
  {
    return Some(Danger::Sensitive(sensitive));
  }

  BROAD_PATHS
    .iter()
    .map(|raw| resolver.resolve(raw))
    .find(|broad| path == broad)
    .map(Danger::Broad)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn resolver() -> PathResolver {
    PathResolver::new("/Users/user/workspace/compostbin", "/Users/user")
  }

  #[test]
  fn refuses_paths_holding_credentials() {
    let resolver = resolver();

    assert_eq!(
      danger(Path::new("/Users/user/.ssh"), &resolver),
      Some(Danger::Sensitive("/Users/user/.ssh".into()))
    );
    assert_eq!(
      danger(Path::new("/Users/user/.ssh/id_ed25519"), &resolver),
      Some(Danger::Sensitive("/Users/user/.ssh".into())),
      "a file inside a sensitive directory is just as sensitive"
    );
    assert_eq!(
      danger(Path::new("/etc/ssh"), &resolver),
      Some(Danger::Sensitive("/etc".into()))
    );
  }

  #[test]
  fn refuses_paths_covering_a_whole_account() {
    let resolver = resolver();

    assert_eq!(danger(Path::new("/"), &resolver), Some(Danger::Broad("/".into())));
    assert_eq!(
      danger(Path::new("/Users/user"), &resolver),
      Some(Danger::Broad("/Users/user".into()))
    );
    assert_eq!(
      danger(Path::new("/Users"), &resolver),
      Some(Danger::Broad("/Users".into()))
    );
  }

  #[test]
  fn allows_an_ordinary_project() {
    let resolver = resolver();

    assert_eq!(danger(Path::new("/Users/user/workspace/compostbin"), &resolver), None);
    assert_eq!(
      danger(Path::new("/Users/user/.sshfoo"), &resolver),
      None,
      "a prefix match is not a containment match"
    );
    assert_eq!(danger(Path::new("/Users/user/Documents/notes"), &resolver), None);
  }
}
