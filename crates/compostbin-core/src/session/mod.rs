pub mod briefing;
pub mod credentials;
pub mod record;
pub mod settings;

#[cfg(test)]
pub(crate) mod fixtures;
mod lifecycle;
mod spec;
mod state;

pub use crate::session::lifecycle::Process;
pub use crate::session::spec::{CLAUDE_HOME_TARGET, CLIPBOARD_DISPLAY, SOCKETS_READY_TIMEOUT};

use crate::error::{ManifestError, PathError};
use crate::host::PortEvent;
use crate::manifest::{
  MANIFEST_RELATIVE_PATH, Manifest, PathEntry, TomlFile, USER_CONFIG_PATH, UserConfig, profile_path,
};
use crate::workspace::paths::{PathResolver, root_containing};
use crate::workspace::{Origin, Workspace};
use std::path::{Path, PathBuf};

pub const NAME_PREFIX: &str = "compostbin-";

/// SHA-256 rather than `DefaultHasher`, which is not stable across Rust releases.
fn directory_hash(dir: &Path) -> String {
  use sha2::{Digest, Sha256};
  use std::os::unix::ffi::OsStrExt;

  crate::hex::encode(&Sha256::digest(dir.as_os_str().as_bytes())[..4])
}

/// Port events arrive here only until Claude attaches; after that the relay writes to `ports_log`.
#[derive(Debug)]
pub enum Notice {
  NotInKeychain,
  /// Files copied from the host's `~/.claude`. Never empty.
  Shared(Vec<String>),
  Port(PortEvent),
  Unpacking(String),
  /// Every socket's guest target, when not all existed within `SOCKETS_READY_TIMEOUT`.
  SocketsNotReady(Vec<PathBuf>),
  /// Stops Claude but not other entrypoints.
  SetupFailed {
    line: String,
    code: i32,
  },
  AgentStopped(PathError),
  CleanupFailed(PathError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddOutcome {
  AlreadyMounted { root: PathBuf },
  NeedsRestart(PathEntry),
}

pub struct Session {
  pub manifest: Manifest,
  profile: Option<String>,
  project_dir: PathBuf,
  resolver: PathResolver,
  pub user: UserConfig,
}

impl Session {
  pub fn new(manifest: Manifest, resolver: PathResolver, project_dir: impl Into<PathBuf>) -> Self {
    Self {
      manifest,
      profile: None,
      project_dir: project_dir.into(),
      resolver,
      user: UserConfig::default(),
    }
  }

  /// With `profile`, the project's manifest and local overlay are not read.
  pub fn load(
    profile: Option<&str>,
    resolver: PathResolver,
    project_dir: impl Into<PathBuf>,
  ) -> Result<Self, ManifestError> {
    let project_dir = project_dir.into();
    let manifest = match profile {
      Some(name) => Manifest::parse(&resolver.resolve(&profile_path(name)))?,
      None => Manifest::load(&project_dir.join(MANIFEST_RELATIVE_PATH))?,
    };
    let user = UserConfig::load_if_present(&resolver.resolve(USER_CONFIG_PATH))?.unwrap_or_default();

    Ok(Self {
      manifest,
      profile: profile.map(str::to_string),
      project_dir,
      resolver,
      user,
    })
  }

  pub fn config_path(&self) -> String {
    match &self.profile {
      Some(name) => profile_path(name),
      None => MANIFEST_RELATIVE_PATH.to_string(),
    }
  }

  /// `path` must be canonical, or a symlink out of a root would look contained.
  pub fn add(&mut self, path: &Path, readonly: bool, local: bool) -> AddOutcome {
    if let Some(root) = root_containing(path, &self.resolved_roots()) {
      return AddOutcome::AlreadyMounted {
        root: root.to_path_buf(),
      };
    }

    let entry = PathEntry {
      local,
      readonly,
      source: path.display().to_string(),
      target: None,
    };
    self.manifest.paths.push(entry.clone());

    AddOutcome::NeedsRestart(entry)
  }

  pub fn resolver(&self) -> &PathResolver {
    &self.resolver
  }

  /// Profiled names include a hash of the directory so checkouts sharing a basename get separate containers.
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

  /// Order matters: a guest name goes to the first entry that claims it.
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

  fn resolved_roots(&self) -> Vec<PathBuf> {
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

    let outcome = session.add(Path::new("/Users/user/workspace/other"), false, false);

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

    let outcome = session.add(Path::new("/Users/user/vendor/libfoo"), true, false);

    assert_eq!(outcome, AddOutcome::NeedsRestart(session.manifest.paths[1].clone()));
    assert_eq!(session.manifest.paths[1].source, "/Users/user/vendor/libfoo");
    assert!(session.manifest.paths[1].readonly);
    assert_eq!(session.manifest.paths[1].target, None);
    assert!(
      session.mounts().contains(&Mount {
        readonly: true,
        source: "/Users/user/vendor/libfoo".into(),
        target: "/workspace/libfoo".into(),
      }),
      "the added path must become a mount"
    );
    assert!(!session.manifest.paths[1].local, "add records a shared path");
  }

  #[test]
  fn a_local_path_mounts_and_says_where_it_came_from() {
    let mut session = session();

    session.add(Path::new("/Users/user/vendor/libfoo"), false, true);

    assert!(session.manifest.paths[1].local);
    let entry = session
      .workspace()
      .entries()
      .iter()
      .find(|entry| entry.host == Path::new("/Users/user/vendor/libfoo"))
      .expect("the local path should be mounted")
      .clone();
    assert_eq!(entry.origin, Origin::Local);
    assert_eq!(entry.guest, PathBuf::from("/workspace/libfoo"));
  }

  fn profiled(profile: &str, project: &str) -> (TempDir, Session) {
    let temp = TempDir::new().expect("temp dir");
    let home = temp.path().canonicalize().expect("canonical temp");
    let profiles = home.join(".config/compostbin/profiles");
    std::fs::create_dir_all(&profiles).expect("profiles dir");
    std::fs::write(profiles.join("rust.toml"), profile).expect("profile should write");
    let project_dir = home.join(project);
    std::fs::create_dir_all(project_dir.join(".config")).expect("project dir");
    std::fs::write(project_dir.join(MANIFEST_RELATIVE_PATH), "[container]\ncpus = 1\n").expect("manifest");

    let session = Session::load(Some("rust"), PathResolver::new(&project_dir, &home), &project_dir)
      .expect("the profile should load");

    (temp, session)
  }

  #[test]
  fn a_profile_replaces_the_projects_manifest() {
    let (_temp, session) = profiled("[container]\ncpus = 6\n", "code/api");

    assert_eq!(session.manifest.container.cpus, 6);
    assert_eq!(session.config_path(), "~/.config/compostbin/profiles/rust.toml");
  }

  #[test]
  fn without_a_profile_the_projects_manifest_configures_it() {
    let temp = TempDir::new().expect("temp dir");
    let project_dir = temp.path().join("api");
    std::fs::create_dir_all(project_dir.join(".config")).expect("project dir");
    std::fs::write(project_dir.join(MANIFEST_RELATIVE_PATH), "[container]\ncpus = 1\n").expect("manifest");

    let session = Session::load(None, PathResolver::new(&project_dir, temp.path()), &project_dir)
      .expect("the manifest should load");

    assert_eq!(session.manifest.container.cpus, 1);
    assert_eq!(session.config_path(), MANIFEST_RELATIVE_PATH);
  }

  #[test]
  fn a_missing_profile_names_its_file() {
    let temp = TempDir::new().expect("temp dir");

    let Err(error) = Session::load(Some("absent"), PathResolver::new(temp.path(), temp.path()), temp.path()) else {
      panic!("a profile that is not there should not load");
    };

    assert!(error.to_string().contains("profiles/absent.toml"), "{error}");
  }

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

  /// Changing the hash strands existing containers and conversations.
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
