//! Which host paths a session can see, and where they land in the guest.
//!
//! `paths` beneath turns what the manifest says into host paths; `danger` on top
//! decides which of those should not be mounted at all.

pub mod danger;
pub mod paths;

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// Everything the session mounts lands under this one guest directory, so no
/// host path — and so no host username or directory layout — is visible inside
/// the container.
pub const WORKSPACE_TARGET: &str = "/workspace";

/// Why a host path is in the workspace — all `ls` needs to explain a mount.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Origin {
  /// A `[[paths]]` entry: mounted because it was asked for by name.
  Explicit,
  /// The directory `compostbin` was invoked in.
  Project,
  /// A `[workspace] roots` entry.
  Root,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
  pub guest: PathBuf,
  pub host: PathBuf,
  pub origin: Origin,
  pub readonly: bool,
}

/// A host symlink inside a mounted tree whose target lies outside every mounted
/// tree. It reads perfectly well on the host and is dead in the container, so
/// the path exists and simply is not there.
#[derive(Clone, Debug, PartialEq)]
pub struct Escape {
  pub link: PathBuf,
  pub target: PathBuf,
}

/// What a walk of the mounted trees found. `exhausted` matters as much as the
/// escapes: a root with a `target/` or `node_modules` under it has no useful
/// bound, so the walk stops rather than costing a minute of `doctor`, and says so
/// instead of reporting a clean tree it never finished reading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Escapes {
  pub escapes: Vec<Escape>,
  pub exhausted: bool,
}

/// Directory entries `escaping_symlinks` will look at before giving up. Large
/// enough for an ordinary source tree, small enough to stay imperceptible.
pub const WALK_LIMIT: usize = 50_000;

/// The host paths a session exposes, each with the `/workspace` path it appears
/// at in the guest.
///
/// Not a symlink farm. Assembling one tree out of many by symlinking the real
/// projects into a session directory does not work: a host symlink pointing out
/// of a mounted tree dangles in the container. So each entry is its own bind
/// mount, assembled in the argv — which is why adding a path requires recreating
/// the container.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
  entries: Vec<Entry>,
}

impl Workspace {
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds `host` under a `/workspace` name derived from its basename, or at
  /// `target` when one was declared. Later entries never take a name an earlier
  /// one holds — a second `libfoo` becomes `libfoo-2` — so two projects sharing a
  /// basename can both be mounted.
  pub fn push(&mut self, host: impl Into<PathBuf>, target: Option<PathBuf>, origin: Origin, readonly: bool) {
    let host = host.into();
    let guest = match target {
      Some(target) => target,
      None => Path::new(WORKSPACE_TARGET).join(self.unique_name(&host)),
    };

    self.entries.push(Entry {
      guest,
      host,
      origin,
      readonly,
    });
  }

  pub fn entries(&self) -> &[Entry] {
    &self.entries
  }

  /// The guest path for a host path in or under one of the entries. `None` when
  /// nothing mounted covers it, which no guess can fix.
  pub fn guest_path(&self, host: &Path) -> Option<PathBuf> {
    self.entries.iter().find_map(|entry| {
      host
        .strip_prefix(&entry.host)
        .ok()
        .map(|relative| entry.guest.join(relative))
    })
  }

  /// Every symlink in a mounted tree that escapes every mounted tree.
  ///
  /// Symlinks are found, never followed: a link to a directory is reported and
  /// not descended into, which bounds the walk against cycles and matches what
  /// the container sees. A link broken on the host too is skipped — equally
  /// broken in both places is not a divergence.
  pub fn escaping_symlinks(&self, limit: usize) -> Escapes {
    let mut found = Escapes::default();
    let mut budget = limit;
    let mut pending: VecDeque<PathBuf> = self.outermost().into_iter().collect();

    while let Some(directory) = pending.pop_front() {
      let Ok(children) = std::fs::read_dir(&directory) else {
        continue;
      };

      for child in children.flatten() {
        if budget == 0 {
          found.exhausted = true;
          return found;
        }
        budget -= 1;

        let path = child.path();
        let Ok(kind) = child.file_type() else { continue };

        if kind.is_symlink() {
          if let Ok(target) = path.canonicalize()
            && self.guest_path(&target).is_none()
          {
            found.escapes.push(Escape { link: path, target });
          }
          continue;
        }

        if kind.is_dir() {
          pending.push_back(path);
        }
      }
    }

    found
  }

  /// Each mounted tree once: an entry inside another, or repeating one, is
  /// already covered by the walk of the outer tree.
  fn outermost(&self) -> BTreeSet<PathBuf> {
    self
      .entries
      .iter()
      .map(|entry| &entry.host)
      .filter(|host| {
        !self
          .entries
          .iter()
          .any(|other| other.host != **host && host.starts_with(&other.host))
      })
      .cloned()
      .collect()
  }

  /// `<basename>`, or `<basename>-2`, `-3`, … if taken. A path with no basename
  /// (`/`) is named `root`, which `doctor` complains about long before it is
  /// mounted.
  fn unique_name(&self, host: &Path) -> String {
    let base = host
      .file_name()
      .map(|name| name.to_string_lossy().into_owned())
      .unwrap_or_else(|| "root".to_string());

    let taken: BTreeSet<&Path> = self
      .entries
      .iter()
      .map(|entry| entry.guest.as_path())
      .collect();
    let workspace = Path::new(WORKSPACE_TARGET);

    if !taken.contains(workspace.join(&base).as_path()) {
      return base;
    }

    (2..)
      .map(|suffix| format!("{base}-{suffix}"))
      .find(|name| !taken.contains(workspace.join(name).as_path()))
      .expect("an unbounded sequence always yields a free name")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  /// A canonical temp root, so macOS's `/var` -> `/private/var` symlink does not
  /// turn every path in these assertions into an escape.
  fn temp_root() -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("temp dir");
    let root = temp.path().canonicalize().expect("canonical temp root");
    (temp, root)
  }

  #[test]
  fn finds_a_symlink_escaping_every_mounted_tree() {
    let (_temp, root) = temp_root();
    std::fs::create_dir_all(root.join("project/src")).expect("create project");
    std::fs::create_dir(root.join("elsewhere")).expect("create elsewhere");
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("project/src/vendor")).expect("create symlink");

    let mut workspace = Workspace::new();
    workspace.push(root.join("project"), None, Origin::Project, false);

    assert_eq!(
      workspace.escaping_symlinks(WALK_LIMIT),
      Escapes {
        escapes: vec![Escape {
          link: root.join("project/src/vendor"),
          target: root.join("elsewhere"),
        }],
        exhausted: false,
      }
    );
  }

  #[test]
  fn accepts_a_symlink_landing_in_another_mounted_tree() {
    let (_temp, root) = temp_root();
    std::fs::create_dir(root.join("project")).expect("create project");
    std::fs::create_dir(root.join("libfoo")).expect("create libfoo");
    std::os::unix::fs::symlink(root.join("libfoo"), root.join("project/vendor")).expect("create symlink");

    let mut workspace = Workspace::new();
    workspace.push(root.join("project"), None, Origin::Project, false);
    workspace.push(root.join("libfoo"), None, Origin::Explicit, true);

    assert_eq!(
      workspace.escaping_symlinks(WALK_LIMIT),
      Escapes::default(),
      "the target is mounted too, so the link is live in the container"
    );
  }

  #[test]
  fn ignores_a_link_that_is_broken_on_the_host_as_well() {
    let (_temp, root) = temp_root();
    std::fs::create_dir(root.join("project")).expect("create project");
    std::os::unix::fs::symlink(root.join("gone"), root.join("project/dead")).expect("create symlink");

    let mut workspace = Workspace::new();
    workspace.push(root.join("project"), None, Origin::Project, false);

    assert_eq!(
      workspace.escaping_symlinks(WALK_LIMIT),
      Escapes::default(),
      "equally broken in both places is not a divergence"
    );
  }

  #[test]
  fn never_follows_a_symlinked_directory() {
    let (_temp, root) = temp_root();
    std::fs::create_dir(root.join("project")).expect("create project");
    std::os::unix::fs::symlink(root.join("project"), root.join("project/loop")).expect("create symlink");

    let mut workspace = Workspace::new();
    workspace.push(root.join("project"), None, Origin::Project, false);

    assert_eq!(
      workspace.escaping_symlinks(WALK_LIMIT),
      Escapes::default(),
      "a link back into the tree is fine, and descending it would never terminate"
    );
  }

  /// The project directory usually sits inside a root, and a hand-edited
  /// `[[paths]]` entry can too: its links are still one set of links.
  #[test]
  fn walks_a_nested_entry_once() {
    let (_temp, root) = temp_root();
    std::fs::create_dir_all(root.join("workspace/project")).expect("create project");
    std::fs::create_dir(root.join("elsewhere")).expect("create elsewhere");
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("workspace/project/vendor")).expect("create symlink");

    let mut workspace = Workspace::new();
    workspace.push(root.join("workspace/project"), None, Origin::Project, false);
    workspace.push(root.join("workspace"), None, Origin::Root, false);
    workspace.push(root.join("workspace/project"), None, Origin::Explicit, true);

    assert_eq!(
      workspace.escaping_symlinks(WALK_LIMIT).escapes,
      [Escape {
        link: root.join("workspace/project/vendor"),
        target: root.join("elsewhere"),
      }]
    );
  }

  #[test]
  fn stops_walking_at_the_limit_and_says_so() {
    let (_temp, root) = temp_root();
    std::fs::create_dir(root.join("project")).expect("create project");
    for index in 0..8 {
      std::fs::write(root.join("project").join(format!("file-{index}")), "").expect("write file");
    }

    let mut workspace = Workspace::new();
    workspace.push(root.join("project"), None, Origin::Project, false);

    let found = workspace.escaping_symlinks(4);
    assert!(
      found.exhausted,
      "a partial answer must not be reported as a clean tree: {found:?}"
    );
  }

  #[test]
  fn mounts_by_basename_under_workspace() {
    let mut workspace = Workspace::new();

    workspace.push("/Users/sax/workspace/compostbin", None, Origin::Project, false);

    assert_eq!(
      workspace.entries(),
      [Entry {
        guest: "/workspace/compostbin".into(),
        host: "/Users/sax/workspace/compostbin".into(),
        origin: Origin::Project,
        readonly: false,
      }]
    );
  }

  #[test]
  fn keeps_the_host_layout_out_of_the_guest_path() {
    let mut workspace = Workspace::new();

    workspace.push("/Users/sax/code/libfoo", None, Origin::Explicit, true);

    let guest = &workspace.entries()[0].guest;
    assert!(
      !guest.display().to_string().contains("sax"),
      "the guest path must not leak the host account: {}",
      guest.display()
    );
  }

  #[test]
  fn disambiguates_a_repeated_basename() {
    let mut workspace = Workspace::new();

    workspace.push("/Users/sax/work/libfoo", None, Origin::Root, false);
    workspace.push("/Users/sax/vendor/libfoo", None, Origin::Explicit, true);
    workspace.push("/opt/libfoo", None, Origin::Explicit, true);

    let guests: Vec<String> = workspace
      .entries()
      .iter()
      .map(|entry| entry.guest.display().to_string())
      .collect();
    assert_eq!(
      guests,
      ["/workspace/libfoo", "/workspace/libfoo-2", "/workspace/libfoo-3"]
    );
  }

  #[test]
  fn honours_a_declared_target() {
    let mut workspace = Workspace::new();

    workspace.push(
      "/Users/sax/code/libfoo",
      Some("/opt/libfoo".into()),
      Origin::Explicit,
      false,
    );

    assert_eq!(workspace.entries()[0].guest, Path::new("/opt/libfoo"));
  }

  #[test]
  fn translates_a_path_under_an_entry() {
    let mut workspace = Workspace::new();
    workspace.push("/Users/sax/workspace", None, Origin::Root, false);

    assert_eq!(
      workspace.guest_path(Path::new("/Users/sax/workspace/compostbin/src")),
      Some(PathBuf::from("/workspace/workspace/compostbin/src"))
    );
    assert_eq!(
      workspace.guest_path(Path::new("/Users/sax/workspace")),
      Some(PathBuf::from("/workspace/workspace"))
    );
  }

  #[test]
  fn translates_nothing_outside_every_entry() {
    let mut workspace = Workspace::new();
    workspace.push("/Users/sax/workspace", None, Origin::Root, false);

    assert_eq!(workspace.guest_path(Path::new("/Users/sax/other")), None);
    assert_eq!(workspace.guest_path(Path::new("/Users/sax/workspace-old")), None);
  }
}
