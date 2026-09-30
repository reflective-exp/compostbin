//! One project's container, and the state that outlives it.
//!
//! The public submodules are what a session owns rather than uses: its image,
//! its token, the host Claude config it starts from, and the record of what its
//! container was created with. The private ones split `Session` itself: `state`
//! is where it keeps things on the host, `spec` what it asks the engine for, and
//! `lifecycle` how it creates, attaches to, and cleans up after its container.

pub mod briefing;
pub mod credentials;
pub mod image;
pub mod record;
pub mod settings;

#[cfg(test)]
mod fixtures;
mod lifecycle;
mod spec;
mod state;

pub use crate::session::lifecycle::Process;
pub use crate::session::spec::{CLAUDE_HOME_TARGET, CLIPBOARD_DISPLAY, KEEPALIVE_COMMAND};
pub use crate::session::state::{CLAUDE_HOME_DIR, PORTS_DIR, PORTS_LOG, SPOOL_DIR};

use crate::error::{ManifestError, PathError};
use crate::host::PortEvent;
use crate::manifest::{MANIFEST_RELATIVE_PATH, Manifest, PathEntry, profile_path};
use crate::workspace::paths::{PathResolver, root_containing};
use crate::workspace::{Origin, Workspace};
use std::path::{Path, PathBuf};

pub const NAME_PREFIX: &str = "compostbin-";

/// The first eight hex digits of the path's SHA-256: stable across Rust
/// releases, unlike `DefaultHasher`, so a profiled session keeps its name.
fn directory_hash(dir: &Path) -> String {
  use sha2::{Digest, Sha256};
  use std::os::unix::ffi::OsStrExt;

  Sha256::digest(dir.as_os_str().as_bytes())[..4]
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect()
}

/// What a session has to say as it starts, runs, and ends, left to the caller to
/// print: core does not own the terminal.
///
/// Port events only arrive here until Claude is attached. After that the
/// terminal is Claude's, and the relay writes to `ports_log` instead.
#[derive(Debug)]
pub enum Notice {
  /// Claude's home had no token, and the Keychain had none to seed it with.
  NotInKeychain,
  /// What was copied from the host's own `~/.claude`. Never empty.
  Shared(Vec<String>),
  Port(PortEvent),
  /// This image's first run, which unpacks it before the container can start.
  Unpacking(String),
  /// A `[container] setup` line exited non-zero. Stops Claude, not any other
  /// entrypoint.
  SetupFailed {
    line: String,
    code: i32,
  },
  /// The host command agent gave up; the session carries on without it.
  AgentStopped(PathError),
  /// The spool could not be emptied after Claude exited.
  CleanupFailed(PathError),
}

/// What `add` did, and therefore what the caller must do next.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddOutcome {
  /// Already inside a mounted root — visible in the container right now.
  AlreadyMounted { root: PathBuf },
  /// Recorded in the manifest, but invisible until the container is recreated.
  NeedsRestart,
}

pub struct Session {
  pub manifest: Manifest,
  /// Set when a profile replaces the project's manifest.
  profile: Option<String>,
  project_dir: PathBuf,
  resolver: PathResolver,
}

impl Session {
  pub fn new(manifest: Manifest, resolver: PathResolver, project_dir: impl Into<PathBuf>) -> Self {
    Self {
      manifest,
      profile: None,
      project_dir: project_dir.into(),
      resolver,
    }
  }

  /// Configured by profile `name` alone; neither project manifest is read.
  pub fn profiled(name: &str, resolver: PathResolver, project_dir: impl Into<PathBuf>) -> Result<Self, ManifestError> {
    Ok(Self {
      manifest: Manifest::parse(&resolver.resolve(&profile_path(name)))?,
      profile: Some(name.to_string()),
      project_dir: project_dir.into(),
      resolver,
    })
  }

  /// Where this session's configuration lives, as the user would write it.
  pub fn config_path(&self) -> String {
    match &self.profile {
      Some(name) => profile_path(name),
      None => MANIFEST_RELATIVE_PATH.to_string(),
    }
  }

  /// Records a path in the manifest unless a mounted root already covers it.
  /// `path` must be canonical: a symlink out of a root looks contained and is
  /// not. `local` records it in the uncommitted manifest instead of the one the
  /// project shares.
  pub fn add(&mut self, path: impl Into<PathBuf>, readonly: bool, local: bool) -> AddOutcome {
    let path = path.into();

    if let Some(root) = root_containing(&path, &self.resolved_roots()) {
      return AddOutcome::AlreadyMounted {
        root: root.to_path_buf(),
      };
    }

    self.manifest.paths.push(PathEntry {
      local,
      readonly,
      source: path.display().to_string(),
      target: None,
    });

    AddOutcome::NeedsRestart
  }

  pub fn resolver(&self) -> &PathResolver {
    &self.resolver
  }

  /// A profile serves many directories, so its sessions are named after the
  /// directory plus a hash of its path: two checkouts named `api` must not
  /// share a container or conversation.
  pub fn container_name(&self) -> String {
    let basename = || {
      self
        .project_dir
        .file_name()
        .map(|basename| basename.to_string_lossy().into_owned())
        .unwrap_or_default()
    };

    let project = match &self.profile {
      Some(_) => format!("{}-{}", basename(), directory_hash(&self.project_dir)),
      None => self.manifest.project.name.clone().unwrap_or_else(basename),
    };

    format!("{NAME_PREFIX}{project}")
  }

  /// The host paths this session exposes, in declaration order: roots, the
  /// project directory when no root covers it, then explicit `[[paths]]`. Order
  /// matters — a name goes to the first entry that claims it.
  pub fn workspace(&self) -> Workspace {
    let mut workspace = Workspace::default();

    for root in &self.manifest.workspace.roots {
      workspace.push(self.resolver.resolve(root), None, Origin::Root, false);
    }

    if root_containing(&self.project_dir, &self.resolved_roots()).is_none() {
      workspace.push(self.project_dir.clone(), None, Origin::Project, false);
    }

    for entry in &self.manifest.paths {
      let target = entry.target.as_ref().map(PathBuf::from);
      let origin = if entry.local { Origin::Local } else { Origin::Explicit };
      workspace.push(self.resolver.resolve(&entry.source), target, origin, entry.readonly);
    }

    workspace
  }

  pub fn resolved_roots(&self) -> Vec<PathBuf> {
    self
      .manifest
      .workspace
      .roots
      .iter()
      .map(|root| self.resolver.resolve(root))
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::fixtures::session;
  use compostbin_engine::model::Mount;
  use tempfile::TempDir;

  #[test]
  fn add_inside_a_root_changes_nothing() {
    let mut session = session();

    let outcome = session.add("/Users/user/workspace/other", false, false);

    assert_eq!(
      outcome,
      AddOutcome::AlreadyMounted {
        root: "/Users/user/workspace".into()
      }
    );
    assert_eq!(session.manifest.paths.len(), 1, "no new [[paths]] entry");
    assert_eq!(session.mounts().len(), 4, "no new mount");
  }

  #[test]
  fn add_outside_every_root_records_a_path() {
    let mut session = session();

    let outcome = session.add("/Users/user/vendor/libfoo", true, false);

    assert_eq!(outcome, AddOutcome::NeedsRestart);
    assert_eq!(session.manifest.paths[1].source, "/Users/user/vendor/libfoo");
    assert_eq!(session.manifest.paths[1].readonly, true);
    assert_eq!(session.manifest.paths[1].target, None);
    assert!(
      session.mounts().contains(&Mount {
        readonly: true,
        source: "/Users/user/vendor/libfoo".into(),
        target: "/workspace/libfoo".into(),
      }),
      "the added path must become a mount"
    );
    assert_eq!(session.manifest.paths[1].local, false, "add records a shared path");
  }

  /// A local path mounts exactly like any other, and only `ls` tells them apart.
  #[test]
  fn a_local_path_mounts_and_says_where_it_came_from() {
    let mut session = session();

    session.add("/Users/user/vendor/libfoo", false, true);

    assert!(session.manifest.paths[1].local);
    let entry = session
      .workspace()
      .entries()
      .iter()
      .find(|entry| entry.host == PathBuf::from("/Users/user/vendor/libfoo"))
      .expect("the local path should be mounted")
      .clone();
    assert_eq!(entry.origin, Origin::Local);
    assert_eq!(entry.guest, PathBuf::from("/workspace/libfoo"));
  }

  /// A home holding `profile` as `rust`, and a project whose own manifest
  /// disagrees.
  fn profiled(profile: &str, project: &str) -> (TempDir, Session) {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path().canonicalize().expect("canonical temp");
    let profiles = home.join(".config/compostbin/profiles");
    std::fs::create_dir_all(&profiles).expect("profiles dir");
    std::fs::write(profiles.join("rust.toml"), profile).expect("profile should write");
    let project_dir = home.join(project);
    std::fs::create_dir_all(project_dir.join(".config")).expect("project dir");
    std::fs::write(project_dir.join(MANIFEST_RELATIVE_PATH), "[container]\ncpus = 1\n").expect("manifest");

    let session =
      Session::profiled("rust", PathResolver::new(&project_dir, &home), &project_dir).expect("the profile should load");

    (temp, session)
  }

  #[test]
  fn a_profile_replaces_the_projects_manifest() {
    let (_temp, session) = profiled("[container]\ncpus = 6\n", "code/api");

    assert_eq!(session.manifest.container.cpus, 6);
    assert_eq!(session.config_path(), "~/.config/compostbin/profiles/rust.toml");
  }

  #[test]
  fn a_missing_profile_names_its_file() {
    let temp = TempDir::new().expect("temp dir");

    let Err(error) = Session::profiled("absent", PathResolver::new(temp.path(), temp.path()), temp.path()) else {
      panic!("a profile that is not there should not load");
    };

    assert!(error.to_string().contains("profiles/absent.toml"), "{error}");
  }

  /// Neither the profile's `[project] name` nor the basename alone tells
  /// directories apart.
  #[test]
  fn a_profiled_session_is_named_after_its_directory() {
    let profile = "[project]\nname = \"shared\"\n";
    let (_one, first) = profiled(profile, "one/api");
    let (_two, second) = profiled(profile, "two/api");

    let name = first.container_name();

    assert!(name.starts_with("compostbin-api-"), "{name}");
    assert_eq!(name.len(), "compostbin-api-".len() + 8, "{name}");
    assert_ne!(name, second.container_name());
  }

  /// Pinned: a changed name strands the conversation kept under the old one.
  #[test]
  fn directory_hash_is_stable() {
    assert_eq!(directory_hash(Path::new("/Users/user/code/api")), "a2415cbb");
  }

  #[test]
  fn a_profile_shares_one_image() {
    let (_temp, session) = profiled("[image]\npackages = [\"jq\"]\n", "code/api");

    assert_eq!(session.image(), "compostbin/profile-rust:latest");
  }
}
