use crate::error::PathError;
use std::path::{Path, PathBuf};

/// Resolves manifest path strings against an explicit working directory and home
/// directory, so resolution is deterministic and independent of the environment.
pub struct PathResolver {
  cwd: PathBuf,
  home: PathBuf,
}

impl PathResolver {
  pub fn new(cwd: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
    Self {
      cwd: cwd.into(),
      home: home.into(),
    }
  }

  pub fn from_env() -> Result<Self, PathError> {
    let cwd = std::env::current_dir().map_err(|source| PathError::new(".", source))?;
    let home = std::env::var_os("HOME").ok_or_else(|| {
      PathError::new(
        "$HOME",
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME is not set"),
      )
    })?;

    Ok(Self::new(cwd, PathBuf::from(home)))
  }

  /// Resolves, then follows symlinks and normalises `.` and `..` against the real
  /// filesystem. Fails if the path does not exist.
  pub fn canonicalize(&self, raw: &str) -> Result<PathBuf, PathError> {
    let resolved = self.resolve(raw);
    resolved
      .canonicalize()
      .map_err(|source| PathError::new(resolved, source))
  }

  /// Expands a leading `~` and makes the result absolute. `~user` is *not*
  /// expanded — unlike a shell, we treat it as a literal relative path.
  pub fn resolve(&self, raw: &str) -> PathBuf {
    if raw == "~" || raw == "~/" {
      return self.home.clone();
    }

    if let Some(relative_to_home) = raw.strip_prefix("~/") {
      return self.home.join(relative_to_home);
    }

    let path = Path::new(raw);
    if path.is_absolute() {
      path.to_path_buf()
    } else {
      self.cwd.join(path)
    }
  }
}

/// The first root that `path` lies within, or `None` if it lies outside all of
/// them. Comparison is by path component, so `~/workspaces` is not inside
/// `~/workspace` despite the string prefix matching.
pub fn root_containing<'a>(path: &Path, roots: &'a [PathBuf]) -> Option<&'a Path> {
  roots
    .iter()
    .find(|root| path.starts_with(root))
    .map(PathBuf::as_path)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn resolver() -> PathResolver {
    PathResolver::new("/Users/user/workspace/compostbin", "/Users/user")
  }

  /// A temp tree containing `real/file.txt` and a `link` symlink pointing at `real`.
  /// The returned path is canonical, so macOS's `/var` -> `/private/var` symlink does
  /// not leak into the assertions.
  fn linked_tree() -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().canonicalize().expect("canonical temp root");
    std::fs::create_dir(root.join("real")).expect("create real");
    std::fs::write(root.join("real/file.txt"), "contents").expect("write file");
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).expect("create symlink");
    (temp, root)
  }

  #[test]
  fn canonicalizes_symlinks() {
    let (_temp, root) = linked_tree();
    let resolver = PathResolver::new(&root, "/Users/user");

    assert_eq!(
      resolver
        .canonicalize("link/file.txt")
        .expect("should resolve"),
      root.join("real/file.txt")
    );
    assert_eq!(
      resolver
        .canonicalize("real/../real/file.txt")
        .expect("should resolve"),
      root.join("real/file.txt")
    );
  }

  #[test]
  fn reads_cwd_and_home_from_env() {
    let resolver = PathResolver::from_env().expect("environment should provide cwd and home");

    let cwd = std::env::current_dir().expect("cwd");
    let home = std::env::var("HOME").expect("HOME");
    assert_eq!(resolver.resolve("Cargo.toml"), cwd.join("Cargo.toml"));
    assert_eq!(
      resolver.resolve("~/.local/state/compostbin"),
      Path::new(&home).join(".local/state/compostbin")
    );
  }

  #[test]
  fn finds_containing_root() {
    let roots = [PathBuf::from("/Users/user/code"), PathBuf::from("/Users/user/workspace")];

    assert_eq!(
      root_containing(Path::new("/Users/user/workspace/compostbin/src"), &roots),
      Some(Path::new("/Users/user/workspace"))
    );
    assert_eq!(
      root_containing(Path::new("/Users/user/code"), &roots),
      Some(Path::new("/Users/user/code"))
    );
    assert_eq!(root_containing(Path::new("/opt/homebrew"), &roots), None);
  }

  #[test]
  fn rejects_prefix_match() {
    let roots = [PathBuf::from("/Users/user/workspace")];

    assert_eq!(root_containing(Path::new("/Users/user/workspaces/other"), &roots), None);
    assert_eq!(root_containing(Path::new("/Users/user/workspace-old"), &roots), None);
  }

  #[test]
  fn rejects_symlink_escaping_roots() {
    let (_temp, root) = linked_tree();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).expect("create outside");
    std::fs::create_dir(root.join("mounted")).expect("create mounted");
    std::os::unix::fs::symlink(&outside, root.join("mounted/escape")).expect("create symlink");

    let resolver = PathResolver::new(&root, "/Users/user");
    let roots = [root.join("mounted")];

    assert_eq!(
      root_containing(&resolver.resolve("mounted/escape"), &roots),
      Some(root.join("mounted").as_path())
    );
    assert_eq!(
      root_containing(
        &resolver
          .canonicalize("mounted/escape")
          .expect("should resolve"),
        &roots
      ),
      None
    );
  }

  #[test]
  fn errors_on_missing_path() {
    let (_temp, root) = linked_tree();
    let resolver = PathResolver::new(&root, "/Users/user");

    let error = resolver
      .canonicalize("nope/absent")
      .expect_err("missing path should error");

    assert_eq!(error.path(), root.join("nope/absent").as_path());
    assert!(
      error.to_string().contains("nope/absent"),
      "error should name the path: {error}"
    );
  }

  #[test]
  fn expands_tilde() {
    assert_eq!(resolver().resolve("~/workspace"), Path::new("/Users/user/workspace"));
    assert_eq!(resolver().resolve("~"), Path::new("/Users/user"));
    assert_eq!(resolver().resolve("~/"), Path::new("/Users/user"));
  }

  #[test]
  fn makes_relative_absolute() {
    assert_eq!(
      resolver().resolve("src"),
      Path::new("/Users/user/workspace/compostbin/src")
    );
    assert_eq!(
      resolver().resolve("./src"),
      Path::new("/Users/user/workspace/compostbin/./src")
    );
    assert_eq!(
      resolver().resolve("../sibling"),
      Path::new("/Users/user/workspace/compostbin/../sibling")
    );
    assert_eq!(resolver().resolve("/opt/homebrew"), Path::new("/opt/homebrew"));
  }

  #[test]
  fn treats_bare_tilde_prefix_as_literal() {
    assert_eq!(
      resolver().resolve("~workspace"),
      Path::new("/Users/user/workspace/compostbin/~workspace")
    );
  }
}
