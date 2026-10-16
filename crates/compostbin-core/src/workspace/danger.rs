use crate::workspace::paths::PathResolver;
use std::fmt;
use std::path::{Path, PathBuf};

const SENSITIVE_PATHS: [&str; 7] = [
  "/etc",
  "/private/etc",
  "~/.aws",
  "~/.gnupg",
  "~/.kube",
  "~/.ssh",
  "~/Library/Keychains",
];
const BROAD_PATHS: [&str; 6] = ["/", "/Users", "~", "~/Desktop", "~/Documents", "~/Downloads"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Danger {
  Broad(PathBuf),
  Sensitive(PathBuf),
}

impl fmt::Display for Danger {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Broad(path) => write!(
        formatter,
        "{} covers a whole account or machine, so the container can read and rewrite every secret under it",
        path.display()
      ),
      Self::Sensitive(path) => write!(formatter, "{} holds credentials", path.display()),
    }
  }
}

/// Expects a resolved path; this guards against slips and is not a security boundary.
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
