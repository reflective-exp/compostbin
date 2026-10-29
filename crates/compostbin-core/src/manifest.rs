use crate::error::{At, InvalidMemory, ManifestError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULT_CONTAINER_CPUS: u32 = 4;
const DEFAULT_CONTAINER_MEMORY: Memory = Memory::gibibytes(8);
const DEFAULT_HOST_CONCURRENCY: usize = 8;
const DEFAULT_IMAGE: &str = "compostbin/base:latest";
pub const MANIFEST_RELATIVE_PATH: &str = ".config/compostbin.toml";
pub const PROFILES_DIR: &str = "~/.config/compostbin/profiles";
pub const USER_CONFIG_PATH: &str = "~/.config/compostbin/config.toml";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
  pub claude: ClaudeConfig,
  container: ContainerConfig,
  #[serde(skip_serializing_if = "HostConfig::is_empty")]
  pub host: HostConfig,
  #[serde(skip_serializing_if = "ImageConfig::is_empty")]
  pub image: ImageConfig,
  #[serde(skip)]
  local_container: ContainerOverrides,
  #[serde(
    serialize_with = "PathEntry::serialize_shared",
    skip_serializing_if = "PathEntry::none_shared"
  )]
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  pub workspace: WorkspaceConfig,
}

impl Manifest {
  pub fn named_after(project_dir: &Path) -> Self {
    Self {
      project: ProjectConfig {
        name: project_dir
          .file_name()
          .map(|basename| basename.to_string_lossy().into_owned()),
        ..ProjectConfig::default()
      },
      ..Self::default()
    }
  }

  /// Includes the local manifest beside it, if any.
  pub fn load(path: &Path) -> Result<Self, ManifestError> {
    let mut manifest = Self::parse(path)?;

    if let Some(local) = LocalManifest::load_if_present(&local_path(path))? {
      manifest.local_container = local.container;
      manifest.paths.extend(
        local
          .paths
          .into_iter()
          .map(|entry| PathEntry { local: true, ..entry }),
      );
    }

    Ok(manifest)
  }

  pub fn parse(path: &Path) -> Result<Self, ManifestError> {
    parse_toml(path, &std::fs::read_to_string(path).at(path)?)
  }

  pub fn container(&self) -> ContainerConfig {
    let shared = &self.container;
    let local = &self.local_container;

    ContainerConfig {
      cpus: local.cpus.unwrap_or(shared.cpus),
      env: local.env.clone().unwrap_or_else(|| shared.env.clone()),
      memory: local.memory.unwrap_or(shared.memory),
      setup: local.setup.clone().unwrap_or_else(|| shared.setup.clone()),
    }
  }

  pub fn shared_container_mut(&mut self) -> &mut ContainerConfig {
    &mut self.container
  }

  fn save_local(&self, manifest_path: &Path) -> Result<(), ManifestError> {
    let path = local_path(manifest_path);
    let local = LocalManifest {
      container: self.local_container.clone(),
      paths: self
        .paths
        .iter()
        .filter(|entry| entry.local)
        .cloned()
        .collect(),
    };

    if local.is_empty() {
      return match std::fs::remove_file(&path).at(&path) {
        Err(error) if !error.is_not_found() => Err(error.into()),
        _ => Ok(()),
      };
    }

    local.save(&path)
  }

  pub fn save_to(&self, manifest_path: &Path, local: bool) -> Result<(), ManifestError> {
    if local {
      self.save_local(manifest_path)
    } else {
      self.save(manifest_path)
    }
  }
}

pub fn profile_path(name: &str) -> String {
  format!("{PROFILES_DIR}/{name}.toml")
}

pub trait TomlFile: Serialize + DeserializeOwned {
  fn load_if_present(path: &Path) -> Result<Option<Self>, ManifestError> {
    let text = match std::fs::read_to_string(path).at(path) {
      Ok(text) => text,
      Err(error) if error.is_not_found() => return Ok(None),
      Err(error) => return Err(error.into()),
    };

    parse_toml(path, &text).map(Some)
  }

  fn save(&self, path: &Path) -> Result<(), ManifestError> {
    let rendered = toml::to_string(self).map_err(|source| ManifestError::Render {
      path: path.to_path_buf(),
      source,
    })?;

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).at(parent)?;
    }

    Ok(std::fs::write(path, rendered).at(path)?)
  }
}

fn parse_toml<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T, ManifestError> {
  toml::from_str(text).map_err(|source| ManifestError::Parse {
    path: path.to_path_buf(),
    source,
  })
}

impl TomlFile for Manifest {}
impl TomlFile for LocalManifest {}
impl TomlFile for UserConfig {}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
  pub claude: UserClaudeConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserClaudeConfig {
  pub shared: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct LocalManifest {
  #[serde(skip_serializing_if = "ContainerOverrides::is_empty")]
  container: ContainerOverrides,
  #[serde(
    serialize_with = "PathEntry::serialize_sorted_by_source",
    skip_serializing_if = "Vec::is_empty"
  )]
  paths: Vec<PathEntry>,
}

impl LocalManifest {
  fn is_empty(&self) -> bool {
    self.container.is_empty() && self.paths.is_empty()
  }
}

fn local_path(manifest_path: &Path) -> PathBuf {
  manifest_path.with_extension("local.toml")
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
  /// Overrides the per-session default under `SESSIONS_DIR`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub home: Option<String>,
  pub keychain: bool,
  /// `~/.claude` entries beyond the host settings every session already gets.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub shared: Vec<String>,
}

impl Default for ClaudeConfig {
  fn default() -> Self {
    Self {
      home: None,
      keychain: true,
      shared: Vec::new(),
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerConfig {
  pub cpus: u32,
  pub env: Vec<String>,
  pub memory: Memory,
  /// Run as `claude` in the project directory when the container is created.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub setup: Vec<String>,
}

impl Default for ContainerConfig {
  fn default() -> Self {
    Self {
      cpus: DEFAULT_CONTAINER_CPUS,
      env: Vec::new(),
      memory: DEFAULT_CONTAINER_MEMORY,
      setup: Vec::new(),
    }
  }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct ContainerOverrides {
  #[serde(skip_serializing_if = "Option::is_none")]
  cpus: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  env: Option<Vec<String>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  memory: Option<Memory>,
  #[serde(skip_serializing_if = "Option::is_none")]
  setup: Option<Vec<String>>,
}

impl ContainerOverrides {
  fn is_empty(&self) -> bool {
    self.cpus.is_none() && self.env.is_none() && self.memory.is_none() && self.setup.is_none()
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Memory(u64);

/// `Display` relies on largest-first order.
const MEMORY_UNITS: [(char, u64); 3] = [('G', 1 << 30), ('M', 1 << 20), ('K', 1 << 10)];

impl Memory {
  pub const fn gibibytes(count: u64) -> Self {
    Self(count << 30)
  }

  pub fn bytes(self) -> u64 {
    self.0
  }
}

impl std::str::FromStr for Memory {
  type Err = InvalidMemory;

  fn from_str(text: &str) -> Result<Self, InvalidMemory> {
    let invalid = || InvalidMemory(text.to_string());
    let trimmed = text.trim();

    let (digits, scale) = match trimmed.char_indices().last() {
      Some((at, unit)) if unit.is_ascii_alphabetic() => {
        let (_, scale) = MEMORY_UNITS
          .iter()
          .find(|(name, _)| name.eq_ignore_ascii_case(&unit))
          .ok_or_else(invalid)?;

        (&trimmed[..at], *scale)
      }
      _ => (trimmed, 1),
    };

    digits
      .trim()
      .parse::<u64>()
      .ok()
      .and_then(|count| count.checked_mul(scale))
      .filter(|&bytes| bytes > 0)
      .map(Self)
      .ok_or_else(invalid)
  }
}

impl TryFrom<String> for Memory {
  type Error = InvalidMemory;

  fn try_from(text: String) -> Result<Self, InvalidMemory> {
    text.parse()
  }
}

impl From<Memory> for String {
  fn from(memory: Memory) -> Self {
    memory.to_string()
  }
}

impl std::fmt::Display for Memory {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match MEMORY_UNITS
      .iter()
      .find(|(_, scale)| self.0.is_multiple_of(*scale))
    {
      Some((unit, scale)) => write!(formatter, "{}{unit}", self.0 / scale),
      None => write!(formatter, "{}", self.0),
    }
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub clipboard: bool,
  pub concurrency: usize,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub ports: Vec<u16>,
  /// Must stay last: TOML cannot express a bare value after a table.
  pub commands: BTreeMap<String, HostCommand>,
}

impl Default for HostConfig {
  fn default() -> Self {
    Self {
      clipboard: false,
      commands: BTreeMap::new(),
      concurrency: DEFAULT_HOST_CONCURRENCY,
      ports: Vec::new(),
    }
  }
}

impl HostConfig {
  pub fn has_commands(&self) -> bool {
    self.clipboard || !self.commands.is_empty()
  }

  pub fn has_ports(&self) -> bool {
    !self.ports.is_empty()
  }

  pub fn is_empty(&self) -> bool {
    !self.has_commands() && !self.has_ports()
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommand {
  #[serde(default)]
  pub arguments: bool,
  pub argv: Vec<String>,
  /// `--config` also refuses `--config=x`, and `-Z` also refuses `-Zx`.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny: Vec<String>,
  /// A pty merges stdout and stderr.
  #[serde(default)]
  pub tty: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
  pub packages: Vec<String>,
  /// Run as the `claude` user.
  pub run: Vec<String>,
  /// Run as root, after `packages` and before `run`.
  pub run_as_root: Vec<String>,
}

impl ImageConfig {
  pub fn is_empty(&self) -> bool {
    self.packages.is_empty() && self.run.is_empty() && self.run_as_root.is_empty()
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
  #[serde(skip)]
  pub local: bool,
  #[serde(default)]
  pub readonly: bool,
  pub source: String,
  /// A guest path; defaults to `/workspace/<basename>`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target: Option<String>,
}

impl PathEntry {
  fn sort_by_source(mut entries: Vec<&PathEntry>) -> Vec<&PathEntry> {
    entries.sort_by(|left, right| left.source.cmp(&right.source));
    entries
  }

  fn serialize_sorted_by_source<S: Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    Self::sort_by_source(entries.iter().collect()).serialize(serializer)
  }

  fn serialize_shared<S: Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    Self::sort_by_source(entries.iter().filter(|entry| !entry.local).collect()).serialize(serializer)
  }

  fn none_shared(entries: &[PathEntry]) -> bool {
    entries.iter().all(|entry| entry.local)
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
  pub image: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub name: Option<String>,
}

impl Default for ProjectConfig {
  fn default() -> Self {
    Self {
      image: DEFAULT_IMAGE.to_string(),
      name: None,
    }
  }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
  pub roots: Vec<String>,
}

#[cfg(test)]
impl Manifest {
  /// No `..Default::default()`, so a new manifest field fails to compile here
  /// until it is documented.
  pub(crate) fn every_key() -> Self {
    Self {
      claude: ClaudeConfig {
        home: Some("~/home".to_string()),
        keychain: true,
        shared: vec!["agents".to_string()],
      },
      container: ContainerConfig {
        cpus: 4,
        env: vec!["TOKEN".to_string()],
        memory: Memory::gibibytes(8),
        setup: vec!["true".to_string()],
      },
      host: HostConfig {
        clipboard: true,
        concurrency: 8,
        ports: vec![7001],
        commands: [(
          "test".to_string(),
          HostCommand {
            arguments: true,
            argv: vec!["true".to_string()],
            deny: vec!["-Z".to_string()],
            tty: true,
          },
        )]
        .into(),
      },
      image: ImageConfig {
        packages: vec!["jq".to_string()],
        run: vec!["true".to_string()],
        run_as_root: vec!["true".to_string()],
      },
      local_container: ContainerOverrides::default(),
      paths: vec![PathEntry {
        local: false,
        readonly: true,
        source: "~/src".to_string(),
        target: Some("/src".to_string()),
      }],
      project: ProjectConfig {
        image: "image".to_string(),
        name: Some("name".to_string()),
      },
      workspace: WorkspaceConfig {
        roots: vec!["~/workspace".to_string()],
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  const FULL_MANIFEST: &str = r#"
[project]
image = "compostbin/base:latest"
name  = "compostbin"

[container]
cpus   = 4
env    = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
memory = "8G"
setup  = ["./bin/setup"]

[workspace]
roots = ["~/workspace"]

[[paths]]
readonly = true
source   = "~/.cargo/registry"

[[paths]]
source = "~/code/vendor/libfoo"
target = "~/code/vendor/libfoo"

[claude]
keychain = true
shared   = ["widgets"]

[image]
packages    = ["jq"]
run         = ["echo 'alias ll=\"ls -l\"' >> ~/.bashrc"]
run_as_root = ["install -d -o claude /opt/vendor"]
"#;

  const EXPECTED_RENDERING: &str = r#"[claude]
keychain = true
shared = ["widgets"]

[container]
cpus = 4
env = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
memory = "8G"
setup = ["./bin/setup"]

[image]
packages = ["jq"]
run = ["""echo 'alias ll="ls -l"' >> ~/.bashrc"""]
run_as_root = ["install -d -o claude /opt/vendor"]

[[paths]]
readonly = true
source = "~/.cargo/registry"

[[paths]]
readonly = false
source = "~/code/vendor/libfoo"
target = "~/code/vendor/libfoo"

[project]
image = "compostbin/base:latest"
name = "compostbin"

[workspace]
roots = ["~/workspace"]
"#;

  const HOST_MANIFEST: &str = r#"
[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

[host.commands.test-one]
arguments = true
argv = ["cargo", "nextest", "run"]
tty = true
"#;

  #[test]
  fn parses_host_commands_with_their_defaults() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    let exact = &manifest.host.commands["test"];
    assert_eq!(exact.argv, ["cargo", "nextest", "run", "--workspace"]);
    assert!(!exact.arguments, "arguments should be off unless asked for");
    assert!(!exact.tty, "a tty should be off unless asked for");

    let widened = &manifest.host.commands["test-one"];
    assert!(widened.arguments);
    assert!(widened.tty);
    assert_eq!(manifest.host.concurrency, DEFAULT_HOST_CONCURRENCY);
  }

  #[test]
  fn ports_default_empty() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    assert!(manifest.host.ports.is_empty());
    assert!(manifest.host.has_commands());
    assert!(!manifest.host.has_ports());
  }

  #[test]
  fn ports_alone_open_no_spool() {
    let manifest: Manifest = toml::from_str("[host]\nports = [7001, 7002]\n").expect("should parse");

    assert_eq!(manifest.host.ports, [7001, 7002]);
    assert!(manifest.host.has_ports());
    assert!(!manifest.host.has_commands(), "ports must not open the spool");

    let rendered = toml::to_string(&manifest).expect("should serialize");
    assert!(
      rendered.contains("[host]\nconcurrency = 8\nports = [7001, 7002]\n"),
      "a table declaring only ports must still render: {rendered}"
    );
  }

  #[test]
  fn saves_ports_and_commands() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("compostbin.toml");
    let mut manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");
    manifest.host.ports = vec![7001];

    manifest.save(&path).expect("save should succeed");

    let reloaded = Manifest::load(&path).expect("load should succeed");
    assert_eq!(reloaded.host.ports, [7001]);
    assert_eq!(reloaded.host.commands.len(), 2);
  }

  #[test]
  fn clipboard_alone_opens_the_spool() {
    let manifest: Manifest = toml::from_str("[host]\nclipboard = true\n").expect("should parse");

    assert!(manifest.host.has_commands());

    let rendered = toml::to_string(&manifest).expect("should serialize");
    assert!(rendered.contains("[host]\nclipboard = true\n"), "{rendered}");
  }

  #[test]
  fn renders_no_clipboard_unless_asked() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    assert!(
      !toml::to_string(&manifest)
        .expect("should serialize")
        .contains("clipboard")
    );
  }

  #[test]
  fn omits_empty_ports() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    let rendered = toml::to_string(&manifest).expect("should serialize");

    assert!(!rendered.contains("ports"), "{rendered}");
  }

  #[test]
  fn names_a_new_manifest_after_the_project_directory() {
    let manifest = Manifest::named_after(Path::new("/home/someone/code/my-project"));

    assert_eq!(manifest.project.name.as_deref(), Some("my-project"));
    assert_eq!(manifest.project.image, DEFAULT_IMAGE);
  }

  #[test]
  fn leaves_a_manifest_unnamed_when_the_directory_has_no_basename() {
    assert_eq!(Manifest::named_after(Path::new("/")).project.name, None);
  }

  #[test]
  fn saves_a_manifest_that_declares_host_commands() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join("compostbin.toml");
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    manifest.save(&path).expect("save should succeed");

    let reloaded = Manifest::load(&path).expect("load should succeed");
    assert_eq!(reloaded.host.commands.len(), 2);
    assert_eq!(reloaded.host.commands["test"].argv, manifest.host.commands["test"].argv);
  }

  #[test]
  fn saves_and_loads() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");

    manifest.save(&path).expect("save should succeed");

    assert_eq!(
      std::fs::read_to_string(&path).expect("manifest should exist"),
      EXPECTED_RENDERING
    );
    assert_eq!(
      Manifest::load(&path)
        .expect("load should succeed")
        .project
        .name
        .as_deref(),
      Some("compostbin")
    );
  }

  #[test]
  fn loads_local_paths_beside_the_manifest() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");
    manifest.save(&path).expect("save should succeed");
    std::fs::write(
      local_path(&path),
      "[[paths]]\nreadonly = true\nsource = \"~/scratch\"\n",
    )
    .expect("local manifest should write");

    let loaded = Manifest::load(&path).expect("load should succeed");

    assert_eq!(loaded.paths.len(), 3);
    let local: Vec<&PathEntry> = loaded.paths.iter().filter(|entry| entry.local).collect();
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].source, "~/scratch");
    assert!(local[0].readonly);

    loaded.save(&path).expect("save should succeed");
    let rendered = std::fs::read_to_string(&path).expect("manifest should exist");
    assert!(
      !rendered.contains("~/scratch"),
      "a local path must not reach the committed manifest: {rendered}"
    );
    assert_eq!(rendered, EXPECTED_RENDERING);
  }

  #[test]
  fn renders_no_paths_when_every_one_is_local() {
    let mut manifest = Manifest::default();
    manifest.paths.push(PathEntry {
      local: true,
      readonly: false,
      source: "~/scratch".to_string(),
      target: None,
    });

    let rendered = toml::to_string(&manifest).expect("should serialize");

    assert!(!rendered.contains("paths"), "{rendered}");
  }

  #[test]
  fn save_to_writes_only_the_file_the_new_entry_belongs_to() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let mut manifest = Manifest::default();
    manifest.save(&path).expect("save should succeed");
    let committed = std::fs::read_to_string(&path).expect("manifest should exist");
    manifest.paths.push(PathEntry {
      local: true,
      readonly: false,
      source: "~/scratch".to_string(),
      target: None,
    });

    manifest.save_to(&path, true).expect("save should succeed");

    assert_eq!(std::fs::read_to_string(&path).expect("manifest"), committed);
    assert!(
      std::fs::read_to_string(local_path(&path))
        .expect("local manifest should exist")
        .contains("~/scratch")
    );
  }

  #[test]
  fn saves_local_paths_to_their_own_file() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let mut manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");
    manifest.save(&path).expect("save should succeed");
    manifest.paths.push(PathEntry {
      local: true,
      readonly: false,
      source: "~/scratch".to_string(),
      target: None,
    });

    manifest.save_local(&path).expect("save should succeed");

    let reloaded = Manifest::load(&path).expect("load should succeed");
    assert_eq!(reloaded.paths.len(), 3);
    assert!(
      reloaded
        .paths
        .iter()
        .any(|entry| entry.local && entry.source == "~/scratch")
    );

    manifest.paths.retain(|entry| !entry.local);
    manifest.save_local(&path).expect("save should succeed");
    assert!(!local_path(&path).exists());
  }

  #[test]
  fn local_container_keys_replace_the_shared_ones() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");
    manifest.save(&path).expect("save should succeed");
    std::fs::write(local_path(&path), "[container]\ncpus = 8\nenv = [\"OTHER_TOKEN\"]\n")
      .expect("local manifest should write");

    let loaded = Manifest::load(&path).expect("load should succeed");
    let container = loaded.container();

    assert_eq!(container.cpus, 8);
    assert_eq!(container.env, ["OTHER_TOKEN"]);
    assert_eq!(container.memory, Memory::gibibytes(8));
    assert_eq!(container.setup, ["./bin/setup"]);
  }

  #[test]
  fn local_container_keys_stay_out_of_the_shared_manifest() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");
    manifest.save(&path).expect("save should succeed");
    let local = "[container]\ncpus = 8\nmemory = \"16G\"\n";
    std::fs::write(local_path(&path), local).expect("local manifest should write");
    let mut loaded = Manifest::load(&path).expect("load should succeed");

    loaded.save(&path).expect("save should succeed");
    assert_eq!(
      std::fs::read_to_string(&path).expect("manifest should exist"),
      EXPECTED_RENDERING
    );

    loaded.paths.push(PathEntry {
      local: true,
      readonly: false,
      source: "~/scratch".to_string(),
      target: None,
    });
    loaded.save_local(&path).expect("save should succeed");
    loaded.paths.retain(|entry| !entry.local);
    loaded.save_local(&path).expect("save should succeed");
    assert_eq!(
      std::fs::read_to_string(local_path(&path)).expect("local overrides should survive"),
      local
    );
  }

  #[test]
  fn a_local_manifest_takes_container_and_paths_only() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    Manifest::default()
      .save(&path)
      .expect("save should succeed");
    std::fs::write(local_path(&path), "[host]\nclipboard = true\n").expect("local manifest should write");

    let error = Manifest::load(&path).expect_err("anything else should be refused");

    assert!(
      error.to_string().contains("compostbin.local.toml"),
      "error should name the local manifest: {error}"
    );
  }

  #[test]
  fn load_names_a_missing_file() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);

    let error = Manifest::load(&path).expect_err("missing manifest should error");

    assert!(
      error.to_string().contains("compostbin.toml"),
      "error should name the manifest: {error}"
    );
  }

  #[test]
  fn parses_all_keys() {
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");

    assert_eq!(manifest.project.image, "compostbin/base:latest");
    assert_eq!(manifest.project.name.as_deref(), Some("compostbin"));

    assert_eq!(manifest.container.cpus, 4);
    assert_eq!(manifest.container.env, ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]);
    assert_eq!(manifest.container.memory, Memory::gibibytes(8));
    assert_eq!(manifest.container.setup, ["./bin/setup"]);

    assert_eq!(manifest.workspace.roots, ["~/workspace"]);

    assert!(manifest.paths[0].readonly);
    assert_eq!(manifest.paths[0].source, "~/.cargo/registry");
    assert_eq!(manifest.paths[0].target, None);
    assert!(!manifest.paths[1].readonly);
    assert_eq!(manifest.paths[1].source, "~/code/vendor/libfoo");
    assert_eq!(manifest.paths[1].target.as_deref(), Some("~/code/vendor/libfoo"));
    assert_eq!(manifest.paths.len(), 2);

    assert_eq!(manifest.claude.home, None);
    assert!(manifest.claude.keychain);
    assert_eq!(manifest.claude.shared, ["widgets"]);

    assert_eq!(manifest.image.packages, ["jq"]);
    assert_eq!(manifest.image.run.len(), 1);
    assert_eq!(manifest.image.run_as_root, ["install -d -o claude /opt/vendor"]);
  }

  #[test]
  fn user_config_takes_claude_shared() {
    let config: UserConfig = toml::from_str("[claude]\nshared = [\"widgets\"]\n").expect("config should parse");

    assert_eq!(config.claude.shared, ["widgets"]);
  }

  #[test]
  fn user_config_refuses_project_settings() {
    assert!(toml::from_str::<UserConfig>("[container]\ncpus = 8\n").is_err());
    assert!(toml::from_str::<UserConfig>("[claude]\nkeychain = false\n").is_err());
  }

  #[test]
  fn says_nothing_about_sharing_until_a_project_adds_something() {
    let rendered = toml::to_string(&Manifest::default()).expect("manifest should serialize");

    assert!(
      !rendered.contains("shared"),
      "the host's own settings are shared without being named: {rendered}"
    );
  }

  #[test]
  fn round_trips() {
    let parsed: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");
    let rendered = toml::to_string(&parsed).expect("manifest should serialize");
    let reparsed: Manifest = toml::from_str(&rendered).expect("rendered manifest should parse");

    assert_eq!(rendered, EXPECTED_RENDERING);
    assert_eq!(reparsed.container.env, ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]);
    assert_eq!(reparsed.paths[0].source, "~/.cargo/registry");
    assert_eq!(reparsed.paths[1].target.as_deref(), Some("~/code/vendor/libfoo"));
  }

  #[test]
  fn rejects_unknown_key() {
    let error =
      toml::from_str::<Manifest>("[project]\nimagee = \"typo\"\n").expect_err("unknown key should be rejected");

    assert!(
      error.to_string().contains("imagee"),
      "error should name the offending key: {error}"
    );
  }

  #[test]
  fn sorts_paths_by_source() {
    let unsorted = r#"
[[paths]]
source = "~/z-last"

[[paths]]
source = "~/a-first"
"#;
    let parsed: Manifest = toml::from_str(unsorted).expect("manifest should parse");
    let rendered = toml::to_string(&parsed).expect("manifest should serialize");

    let sources: Vec<&str> = rendered
      .lines()
      .filter(|line| line.starts_with("source = "))
      .collect();
    assert_eq!(sources, [r#"source = "~/a-first""#, r#"source = "~/z-last""#]);
  }

  #[test]
  fn reads_the_memory_forms_a_manifest_uses() {
    for (text, bytes) in [("8G", 8 << 30), ("512M", 512 << 20), ("1024k", 1 << 20), ("2048", 2048)] {
      assert_eq!(text.parse::<Memory>().map(Memory::bytes), Ok(bytes), "{text}");
    }
  }

  #[test]
  fn writes_memory_back_the_way_it_reads() {
    for text in ["8G", "512M", "1536K", "1000"] {
      assert_eq!(
        text
          .parse::<Memory>()
          .expect("a size should parse")
          .to_string(),
        text
      );
    }
  }

  #[test]
  fn refuses_memory_that_is_not_a_size() {
    for text in ["", "lots", "8GB", "8T", "0", "-1G"] {
      assert!(text.parse::<Memory>().is_err(), "{text:?} should not parse");
    }

    let error = toml::from_str::<Manifest>("[container]\nmemory = \"8GB\"\n").expect_err("8GB is not a size");

    assert!(error.to_string().contains("\"8GB\" is not a memory size"), "{error}");
  }

  #[test]
  fn applies_defaults() {
    let manifest: Manifest = toml::from_str("").expect("empty manifest should parse");

    assert_eq!(manifest.project.image, "compostbin/base:latest");
    assert_eq!(manifest.project.name, None);

    assert_eq!(manifest.container.cpus, 4);
    assert_eq!(manifest.container.env, [] as [String; 0]);
    assert_eq!(manifest.container.memory, Memory::gibibytes(8));
    assert_eq!(manifest.container.setup, [] as [String; 0]);

    assert_eq!(manifest.workspace.roots, [] as [String; 0]);
    assert_eq!(manifest.paths.len(), 0);

    assert_eq!(
      manifest.claude.home, None,
      "an unset home is what makes the session's home per project"
    );
    assert!(manifest.claude.keychain);
    assert_eq!(
      manifest.claude.shared,
      [] as [String; 0],
      "a project adds to the host's own settings rather than restating them"
    );

    assert!(
      manifest.image.is_empty(),
      "a project with no additions runs the base image itself"
    );
  }
}
