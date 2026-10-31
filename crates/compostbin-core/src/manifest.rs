use crate::error::{At, InvalidMemory, InvalidSocketMode, ManifestError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use toml::{Table, Value};

const DEFAULT_CONTAINER_CPUS: u32 = 4;
const DEFAULT_CONTAINER_MEMORY: Memory = Memory::gibibytes(8);
const DEFAULT_HOST_CONCURRENCY: usize = 8;
const DEFAULT_IMAGE: &str = "compostbin/base:latest";
pub const MANIFEST_RELATIVE_PATH: &str = ".config/compostbin.toml";
pub const PROFILES_DIR: &str = "~/.config/compostbin/profiles";
pub const USER_CONFIG_PATH: &str = "~/.config/compostbin/config.toml";
/// Set by compostbin in exec'd processes, so `[container]` may not name them.
pub const RESERVED_VARIABLES: [&str; 3] = ["CLAUDE_CONFIG_DIR", "IS_SANDBOX", "WAYLAND_DISPLAY"];

/// Renders only what differs from the defaults, except `[project] image`.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
  #[serde(skip_serializing_if = "ClaudeConfig::is_default")]
  pub claude: ClaudeConfig,
  #[serde(skip_serializing_if = "ContainerConfig::is_default")]
  pub container: ContainerConfig,
  #[serde(skip_serializing_if = "HostConfig::is_empty")]
  pub host: HostConfig,
  #[serde(skip_serializing_if = "ImageConfig::is_empty")]
  pub image: ImageConfig,
  #[serde(
    serialize_with = "PathEntry::serialize_sorted_by_source",
    skip_serializing_if = "Vec::is_empty"
  )]
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  #[serde(skip_serializing_if = "WorkspaceConfig::is_default")]
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

  /// Includes the local manifest beside it, if any. Each key it sets replaces the shared one,
  /// tables merging key by key down to a whole host command, and its `[[paths]]` are added.
  pub fn load(path: &Path) -> Result<Self, ManifestError> {
    let mut shared = read_table(path)?;
    let local_path = local_path(path);

    let Some(mut local) = Table::load_if_present(&local_path)? else {
      return from_table(path, shared);
    };

    // Parsed alone too, so a mistake in it names its own file.
    let local_paths = from_table::<Self>(&local_path, with_shared_sockets(&local, &shared))?.paths;
    local.remove(PATHS_KEY);
    overlay(&mut shared, local, &[]);

    let mut manifest: Self = from_table(path, shared)?;
    manifest.paths.extend(
      local_paths
        .into_iter()
        .map(|entry| PathEntry { local: true, ..entry }),
    );

    Ok(manifest)
  }

  pub fn parse(path: &Path) -> Result<Self, ManifestError> {
    from_table(path, read_table(path)?)
  }
}

const PATHS_KEY: &str = "paths";

/// A partial host command could not stand alone, so a local one replaces the shared one whole.
const WHOLE_ENTRIES: &[&str] = &["host", "commands"];

/// `local`, each socket in it given the shared socket's `source` if it sets none. Only `source` is required, and
/// taking no other key keeps a mistake in the shared socket out of an error naming `local`.
fn with_shared_sockets(local: &Table, shared: &Table) -> Table {
  let sockets = |table: &Table| {
    table
      .get("host")
      .and_then(|host| host.get("sockets"))
      .and_then(Value::as_table)
      .cloned()
  };
  let mut local = local.clone();

  if let (Some(base), Some(mut over)) = (sockets(shared), sockets(&local)) {
    for (name, socket) in &mut over {
      let source = base.get(name).and_then(|shared| shared.get(SOURCE_KEY));
      if let (Value::Table(socket), Some(source)) = (socket, source) {
        socket.entry(SOURCE_KEY).or_insert_with(|| source.clone());
      }
    }
    local["host"]["sockets"] = Value::Table(over);
  }

  local
}

const SOURCE_KEY: &str = "source";

fn overlay(shared: &mut Table, local: Table, at: &[&str]) {
  for (key, value) in local {
    let nested: Vec<&str> = at.iter().copied().chain([key.as_str()]).collect();

    match (shared.get_mut(&key), value) {
      (Some(Value::Table(base)), Value::Table(over)) if at != WHOLE_ENTRIES => overlay(base, over, &nested),
      (_, value) => {
        shared.insert(key.clone(), value);
      }
    }
  }
}

/// Edits only the file `entry` belongs in, keeping every other key that file sets.
pub fn record_path(manifest_path: &Path, entry: &PathEntry) -> Result<(), ManifestError> {
  let path = if entry.local {
    local_path(manifest_path)
  } else {
    manifest_path.to_path_buf()
  };

  let mut table = Table::load_if_present(&path)?.unwrap_or_default();
  let mut paths: Vec<PathEntry> = match table.remove(PATHS_KEY) {
    Some(recorded) => recorded.try_into().map_err(|source| ManifestError::Parse {
      path: path.clone(),
      source,
    })?,
    None => Vec::new(),
  };

  paths.push(entry.clone());
  paths.sort_by(|left, right| left.source.cmp(&right.source));

  let rendered = Value::try_from(paths).map_err(|source| ManifestError::Render {
    path: path.clone(),
    source,
  })?;
  table.insert(PATHS_KEY.to_string(), rendered);

  table.save(&path)
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

fn read_table(path: &Path) -> Result<Table, ManifestError> {
  parse_toml(path, &std::fs::read_to_string(path).at(path)?)
}

fn from_table<T: DeserializeOwned>(path: &Path, table: Table) -> Result<T, ManifestError> {
  table.try_into().map_err(|source| ManifestError::Parse {
    path: path.to_path_buf(),
    source,
  })
}

impl TomlFile for Manifest {}
impl TomlFile for Table {}
impl TomlFile for UserConfig {}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserConfig {
  pub claude: UserClaudeConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserClaudeConfig {
  pub keychain: bool,
  pub shared: Vec<String>,
}

pub(crate) fn local_path(manifest_path: &Path) -> PathBuf {
  manifest_path.with_extension("local.toml")
}

#[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
  /// Overrides the per-session default under `SESSIONS_DIR`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub home: Option<String>,
  /// `~/.claude` entries beyond the host settings every session already gets.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub shared: Vec<String>,
}

impl ClaudeConfig {
  fn is_default(&self) -> bool {
    *self == Self::default()
  }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerConfig {
  #[serde(skip_serializing_if = "ContainerConfig::is_default_cpus")]
  pub cpus: u32,
  #[serde(
    deserialize_with = "ContainerConfig::deserialize_env",
    skip_serializing_if = "Vec::is_empty"
  )]
  pub env: Vec<String>,
  /// Variables set to these values, where `env` passes the host's through.
  #[serde(
    deserialize_with = "ContainerConfig::deserialize_environment",
    skip_serializing_if = "BTreeMap::is_empty"
  )]
  pub environment: BTreeMap<String, String>,
  #[serde(skip_serializing_if = "ContainerConfig::is_default_memory")]
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
      environment: BTreeMap::new(),
      memory: DEFAULT_CONTAINER_MEMORY,
      setup: Vec::new(),
    }
  }
}

impl ContainerConfig {
  fn is_default(&self) -> bool {
    *self == Self::default()
  }

  fn is_default_cpus(cpus: &u32) -> bool {
    *cpus == DEFAULT_CONTAINER_CPUS
  }

  fn is_default_memory(memory: &Memory) -> bool {
    *memory == DEFAULT_CONTAINER_MEMORY
  }

  fn deserialize_env<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let env = Vec::<String>::deserialize(deserializer)?;

    if let Some(name) = env.iter().find(|name| is_reserved(name)) {
      return Err(reserved_error(name));
    }

    Ok(env)
  }

  /// A bad name or value would otherwise fail out of sight, as the container starts.
  fn deserialize_environment<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
  ) -> Result<BTreeMap<String, String>, D::Error> {
    let environment = BTreeMap::<String, String>::deserialize(deserializer)?;

    for (name, value) in &environment {
      if !is_variable_name(name) {
        return Err(serde::de::Error::custom(format!(
          "environment variable `{name}` must be letters, digits and `_`, not starting with a digit"
        )));
      }
      if is_reserved(name) {
        return Err(reserved_error(name));
      }
      if value.contains('\0') {
        return Err(serde::de::Error::custom(format!(
          "environment variable `{name}`'s value has a NUL, which no process can be given"
        )));
      }
    }

    Ok(environment)
  }
}

fn is_reserved(name: &str) -> bool {
  RESERVED_VARIABLES.contains(&name)
}

fn reserved_error<E: serde::de::Error>(name: &str) -> E {
  E::custom(format!("environment variable `{name}` is set by compostbin"))
}

fn is_variable_name(name: &str) -> bool {
  name
    .chars()
    .next()
    .is_some_and(|first| !first.is_ascii_digit())
    && name
      .chars()
      .all(|character| character.is_ascii_alphanumeric() || character == '_')
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
  #[serde(skip_serializing_if = "HostConfig::is_default_concurrency")]
  pub concurrency: usize,
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub ports: Vec<u16>,
  #[serde(
    deserialize_with = "HostConfig::deserialize_sockets",
    skip_serializing_if = "BTreeMap::is_empty"
  )]
  pub sockets: BTreeMap<String, HostSocket>,
  /// Must stay last: TOML cannot express a bare value after a table.
  #[serde(skip_serializing_if = "BTreeMap::is_empty")]
  pub commands: BTreeMap<String, HostCommand>,
}

impl Default for HostConfig {
  fn default() -> Self {
    Self {
      clipboard: false,
      commands: BTreeMap::new(),
      concurrency: DEFAULT_HOST_CONCURRENCY,
      ports: Vec::new(),
      sockets: BTreeMap::new(),
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

  pub fn has_sockets(&self) -> bool {
    !self.sockets.is_empty()
  }

  pub fn is_empty(&self) -> bool {
    !self.has_commands() && !self.has_ports() && !self.has_sockets()
  }

  /// Each socket by name, with where it answers in the guest.
  pub fn socket_targets(&self) -> impl Iterator<Item = (PathBuf, &HostSocket)> {
    self
      .sockets
      .iter()
      .map(|(name, socket)| (socket.target(name), socket))
  }

  fn is_default_concurrency(concurrency: &usize) -> bool {
    *concurrency == DEFAULT_HOST_CONCURRENCY
  }

  /// A bad socket would otherwise fail out of sight in the guest relay.
  fn deserialize_sockets<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
  ) -> Result<BTreeMap<String, HostSocket>, D::Error> {
    let sockets = BTreeMap::<String, HostSocket>::deserialize(deserializer)?;
    let mut targets = BTreeSet::new();

    for (name, socket) in &sockets {
      if !is_socket_name(name) {
        return Err(serde::de::Error::custom(format!(
          "socket name `{name}` must be letters, digits, `-`, `_` and `.`, not starting with `.`"
        )));
      }

      let target = socket.target(name);
      // `socat` gives `,`, `:`, `!!`, quotes, backslashes and brackets in an address meanings of their own.
      if !target.is_absolute()
        || target
          .components()
          .any(|component| component == Component::ParentDir)
        || !target
          .to_string_lossy()
          .chars()
          .all(|character| character == '/' || is_name_character(character))
      {
        return Err(serde::de::Error::custom(format!(
          "socket `{name}`'s target `{}` must be an absolute path of letters, digits, `-`, `_`, `.` and `/`, \
           without `..`",
          target.display()
        )));
      }
      if let Some(target) = targets.replace(target) {
        return Err(serde::de::Error::custom(format!(
          "socket `{name}` shares its target `{}` with another socket",
          target.display()
        )));
      }
    }

    Ok(sockets)
  }
}

/// A relayed socket's permissions in the guest.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct SocketMode(u32);

impl SocketMode {
  pub const fn new(bits: u32) -> Self {
    Self(bits)
  }

  fn is_default(&self) -> bool {
    *self == Self::default()
  }
}

impl Default for SocketMode {
  fn default() -> Self {
    Self(0o660)
  }
}

impl std::str::FromStr for SocketMode {
  type Err = InvalidSocketMode;

  fn from_str(text: &str) -> Result<Self, InvalidSocketMode> {
    // `from_str_radix` takes a leading `+`.
    let digits = (3..=4).contains(&text.len()) && !text.starts_with('+');

    u32::from_str_radix(text, 8)
      .ok()
      .filter(|&bits| digits && bits <= 0o777)
      .map(Self)
      .ok_or_else(|| InvalidSocketMode(text.to_string()))
  }
}

impl TryFrom<String> for SocketMode {
  type Error = InvalidSocketMode;

  fn try_from(text: String) -> Result<Self, InvalidSocketMode> {
    text.parse()
  }
}

impl From<SocketMode> for String {
  fn from(mode: SocketMode) -> Self {
    mode.to_string()
  }
}

impl std::fmt::Display for SocketMode {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "{:04o}", self.0)
  }
}

impl std::fmt::Octal for SocketMode {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    std::fmt::Octal::fmt(&self.0, formatter)
  }
}

fn is_socket_name(name: &str) -> bool {
  !name.is_empty() && !name.starts_with('.') && name.chars().all(is_name_character)
}

fn is_name_character(character: char) -> bool {
  character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
}

/// A host socket, relayed to `target` in the guest. It belongs to the `compostbin` group, which the
/// session's user is in.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostSocket {
  pub source: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target: Option<PathBuf>,
  #[serde(default, skip_serializing_if = "SocketMode::is_default")]
  pub mode: SocketMode,
}

impl HostSocket {
  pub fn new(source: impl Into<String>) -> Self {
    Self {
      source: source.into(),
      target: None,
      mode: SocketMode::default(),
    }
  }

  /// Where the socket `name` answers in the guest.
  pub fn target(&self, name: &str) -> PathBuf {
    self
      .target
      .clone()
      .unwrap_or_else(|| Path::new(crate::host::GUEST_SOCKETS_DIR).join(name))
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommand {
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub arguments: bool,
  pub argv: Vec<String>,
  /// `--config` also refuses `--config=x`, and `-Z` also refuses `-Zx`.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny: Vec<String>,
  /// A pty merges stdout and stderr.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub tty: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub packages: Vec<String>,
  /// Run as the `claude` user.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub run: Vec<String>,
  /// Run as root, after `packages` and before `run`.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub run_as_root: Vec<String>,
}

impl ImageConfig {
  pub fn is_empty(&self) -> bool {
    self.packages.is_empty() && self.run.is_empty() && self.run_as_root.is_empty()
  }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
  #[serde(skip)]
  pub local: bool,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub readonly: bool,
  pub source: String,
  /// A guest path; defaults to `/workspace/<basename>`.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target: Option<String>,
}

impl PathEntry {
  fn serialize_sorted_by_source<S: Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    let mut sorted: Vec<&PathEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| left.source.cmp(&right.source));
    sorted.serialize(serializer)
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

impl WorkspaceConfig {
  fn is_default(&self) -> bool {
    self.roots.is_empty()
  }
}

#[cfg(test)]
impl Manifest {
  /// No `..Default::default()`, so a new manifest field fails to compile here
  /// until it is documented.
  pub(crate) fn every_key() -> Self {
    Self {
      claude: ClaudeConfig {
        home: Some("~/home".to_string()),
        shared: vec!["agents".to_string()],
      },
      container: ContainerConfig {
        cpus: 4,
        env: vec!["TOKEN".to_string()],
        environment: [("MODE".to_string(), "fast".to_string())].into(),
        memory: Memory::gibibytes(8),
        setup: vec!["true".to_string()],
      },
      host: HostConfig {
        clipboard: true,
        concurrency: 8,
        ports: vec![7001],
        sockets: [(
          "mcp".to_string(),
          HostSocket {
            source: "~/mcp.sock".to_string(),
            target: Some("/run/mcp.sock".into()),
            mode: SocketMode::new(0o600),
          },
        )]
        .into(),
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
shared = ["widgets"]

[image]
packages    = ["jq"]
run         = ["echo 'alias ll=\"ls -l\"' >> ~/.bashrc"]
run_as_root = ["install -d -o claude /opt/vendor"]
"#;

  const EXPECTED_RENDERING: &str = r#"[claude]
shared = ["widgets"]

[container]
env = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
setup = ["./bin/setup"]

[image]
packages = ["jq"]
run = ["""echo 'alias ll="ls -l"' >> ~/.bashrc"""]
run_as_root = ["install -d -o claude /opt/vendor"]

[[paths]]
readonly = true
source = "~/.cargo/registry"

[[paths]]
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
      rendered.starts_with("[host]\nports = [7001, 7002]\n\n[project]"),
      "a table declaring only ports must still render: {rendered}"
    );
  }

  fn sockets_error(sockets: &str) -> String {
    toml::from_str::<Manifest>(sockets)
      .expect_err("the socket should be refused")
      .to_string()
  }

  #[test]
  fn sockets_alone_open_no_spool() {
    let manifest: Manifest = toml::from_str("[host.sockets.mcp]\nsource = \"~/mcp.sock\"\n").expect("should parse");

    assert_eq!(
      manifest.host.sockets,
      BTreeMap::from([("mcp".to_string(), HostSocket::new("~/mcp.sock"))])
    );
    assert!(manifest.host.has_sockets());
    assert!(!manifest.host.has_commands(), "sockets must not open the spool");

    let rendered = toml::to_string(&manifest).expect("should serialize");
    assert!(
      rendered.contains("[host.sockets.mcp]\nsource = \"~/mcp.sock\"\n"),
      "a table declaring only sockets must still render, without its defaults: {rendered}"
    );
  }

  #[test]
  fn puts_a_socket_in_the_guest_sockets_directory_unless_it_names_a_target() {
    let manifest: Manifest = toml::from_str(
      "[host.sockets.mcp]\nsource = \"~/mcp.sock\"\n\n[host.sockets.other]\nsource = \"/other.sock\"\ntarget = \"/run/other.sock\"\nmode = \"0600\"\n",
    )
    .expect("should parse");

    assert_eq!(
      manifest.host.sockets["mcp"].target("mcp"),
      Path::new("/run/compostbin/sockets/mcp")
    );
    assert_eq!(manifest.host.sockets["mcp"].mode, SocketMode::default());
    assert_eq!(
      manifest.host.sockets["other"].target("other"),
      Path::new("/run/other.sock")
    );
    assert_eq!(manifest.host.sockets["other"].mode, SocketMode::new(0o600));
  }

  #[test]
  fn a_socket_mode_reads_and_prints_as_octal() {
    assert_eq!("660".parse::<SocketMode>(), Ok(SocketMode::new(0o660)));
    assert_eq!(SocketMode::new(0o600).to_string(), "0600");
    assert_eq!(String::from(SocketMode::default()), "0660");
  }

  #[test]
  fn refuses_a_socket_name_unfit_for_a_file_name() {
    for name in ["../escape", ".hidden", "a/b", ""] {
      let error = sockets_error(&format!("[host.sockets.\"{name}\"]\nsource = \"/host.sock\"\n"));

      assert!(error.contains("socket name"), "{name}: {error}");
    }
  }

  #[test]
  fn refuses_a_target_socat_would_misread() {
    for target in [
      "relative.sock",
      "/a,b.sock",
      "/a:b.sock",
      "/a!!b.sock",
      "/a'b.sock",
      "/a b.sock",
      "/a(b).sock",
      "/run/compostbin/sockets/../../../etc/passwd",
    ] {
      let error = sockets_error(&format!(
        "[host.sockets.mcp]\nsource = \"/host.sock\"\ntarget = \"{target}\"\n"
      ));

      assert!(error.contains("absolute path"), "{target}: {error}");
    }
  }

  #[test]
  fn refuses_two_sockets_at_one_target() {
    let error = sockets_error(
      "[host.sockets.a]\nsource = \"/a.sock\"\ntarget = \"/run/compostbin/sockets/b\"\n\n[host.sockets.b]\nsource = \"/b.sock\"\n",
    );

    assert!(error.contains("shares its target"), "{error}");
  }

  #[test]
  fn refuses_a_mode_that_is_not_permissions() {
    for mode in ["rw", "0999", "10000", "66", "+660", "+66"] {
      let error = sockets_error(&format!(
        "[host.sockets.mcp]\nsource = \"/host.sock\"\nmode = \"{mode}\"\n"
      ));

      assert!(error.contains("octal permissions"), "{mode}: {error}");
    }
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
  }

  fn shared_and_local(shared: &str, local: &str) -> (TempDir, PathBuf) {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    std::fs::create_dir_all(path.parent().expect("manifest dir")).expect("create manifest dir");
    std::fs::write(&path, shared).expect("manifest should write");
    std::fs::write(local_path(&path), local).expect("local manifest should write");
    (temp, path)
  }

  #[test]
  fn local_keys_replace_the_shared_ones_in_every_table() {
    let (_temp, path) = shared_and_local(
      FULL_MANIFEST,
      r#"
[claude]
home = "~/elsewhere-home"

[container]
cpus = 8
env  = ["OTHER_TOKEN"]

[host]
clipboard = true

[image]
packages = ["ripgrep"]

[project]
image = "mine:latest"

[workspace]
roots = ["~/elsewhere"]
"#,
    );

    let loaded = Manifest::load(&path).expect("load should succeed");

    assert_eq!(loaded.claude.home.as_deref(), Some("~/elsewhere-home"));
    assert_eq!(
      loaded.claude.shared,
      ["widgets"],
      "a key left out keeps the shared value"
    );
    assert_eq!(loaded.container.cpus, 8);
    assert_eq!(
      loaded.container.env,
      ["OTHER_TOKEN"],
      "a list is replaced, not extended"
    );
    assert_eq!(loaded.container.memory, Memory::gibibytes(8));
    assert_eq!(loaded.container.setup, ["./bin/setup"]);
    assert!(loaded.host.clipboard);
    assert_eq!(loaded.image.packages, ["ripgrep"]);
    assert_eq!(loaded.image.run_as_root, ["install -d -o claude /opt/vendor"]);
    assert_eq!(loaded.project.image, "mine:latest");
    assert_eq!(loaded.project.name.as_deref(), Some("compostbin"));
    assert_eq!(loaded.workspace.roots, ["~/elsewhere"]);
  }

  #[test]
  fn sets_environment_variables_to_values() {
    let manifest: Manifest =
      toml::from_str("[container.environment]\nMODE = \"fast\"\n_UNDER = \"a=b\"\n").expect("should parse");

    assert_eq!(
      manifest.container.environment,
      [
        ("MODE".to_string(), "fast".to_string()),
        ("_UNDER".to_string(), "a=b".to_string()),
      ]
      .into()
    );
  }

  #[test]
  fn refuses_an_environment_variable_no_process_could_have() {
    for name in ["1ST", "A-B", "A B", "A=B", ""] {
      let error = toml::from_str::<Manifest>(&format!("[container.environment]\n\"{name}\" = \"x\"\n"))
        .expect_err("the name should be refused")
        .to_string();

      assert!(error.contains("must be letters"), "{name}: {error}");
    }
    let error = toml::from_str::<Manifest>("[container.environment]\nMODE = \"a\\u0000b\"\n")
      .expect_err("the value should be refused")
      .to_string();
    assert!(error.contains("NUL"), "{error}");
  }

  #[test]
  fn refuses_an_environment_variable_compostbin_sets() {
    for name in RESERVED_VARIABLES {
      for manifest in [
        format!("[container.environment]\n{name} = \"x\"\n"),
        format!("[container]\nenv = [\"{name}\"]\n"),
      ] {
        let error = toml::from_str::<Manifest>(&manifest)
          .expect_err("the name should be refused")
          .to_string();

        assert!(error.contains("is set by compostbin"), "{manifest}: {error}");
      }
    }
  }

  #[test]
  fn a_local_environment_variable_replaces_only_that_variable() {
    let (_temp, path) = shared_and_local(
      "[container.environment]\nKEPT = \"shared\"\nMODE = \"shared\"\n",
      "[container.environment]\nMODE = \"local\"\n",
    );

    let loaded = Manifest::load(&path).expect("load should succeed");

    assert_eq!(
      loaded.container.environment,
      [
        ("KEPT".to_string(), "shared".to_string()),
        ("MODE".to_string(), "local".to_string()),
      ]
      .into()
    );
  }

  const SOCKET_MANIFEST: &str =
    "[host.sockets.mcp]\nsource = \"~/mcp.sock\"\ntarget = \"/run/mcp.sock\"\nmode = \"0600\"\n";

  #[test]
  fn a_local_socket_key_replaces_only_that_key() {
    let (_temp, path) = shared_and_local(
      SOCKET_MANIFEST,
      "[host.sockets.mcp]\nsource = \"~/elsewhere/mcp.sock\"\n\n[host.sockets.mine]\nsource = \"~/mine.sock\"\n",
    );

    let sockets = Manifest::load(&path)
      .expect("load should succeed")
      .host
      .sockets;

    assert_eq!(
      sockets["mcp"],
      HostSocket {
        source: "~/elsewhere/mcp.sock".to_string(),
        target: Some("/run/mcp.sock".into()),
        mode: SocketMode::new(0o600),
      }
    );
    assert_eq!(sockets["mine"], HostSocket::new("~/mine.sock"));
  }

  #[test]
  fn a_local_socket_may_leave_out_its_source() {
    let (_temp, path) = shared_and_local(SOCKET_MANIFEST, "[host.sockets.mcp]\nmode = \"0660\"\n");

    let sockets = Manifest::load(&path)
      .expect("load should succeed")
      .host
      .sockets;

    assert_eq!(sockets["mcp"].source, "~/mcp.sock");
    assert_eq!(sockets["mcp"].mode, SocketMode::default());
  }

  #[test]
  fn a_local_socket_is_checked_against_the_shared_ones() {
    let (_temp, path) = shared_and_local(
      SOCKET_MANIFEST,
      "[host.sockets.other]\nsource = \"/other.sock\"\ntarget = \"/run/mcp.sock\"\n",
    );

    let error = Manifest::load(&path)
      .expect_err("two sockets at one target")
      .to_string();

    assert!(error.contains("shares its target"), "{error}");
  }

  #[test]
  fn a_mistake_in_a_shared_socket_names_the_shared_manifest() {
    let (_temp, path) = shared_and_local(
      "[host.sockets.mcp]\nsource = \"~/mcp.sock\"\nmode = \"rw\"\n",
      "[host.sockets.mcp]\nsource = \"~/elsewhere/mcp.sock\"\n",
    );

    let error = Manifest::load(&path).expect_err("a bad mode").to_string();

    assert!(error.contains("compostbin.toml"), "{error}");
  }

  #[test]
  fn a_local_host_command_replaces_the_shared_one_whole() {
    let (_temp, path) = shared_and_local(
      HOST_MANIFEST,
      "[host.commands.test]\nargv = [\"just\", \"test\"]\n\n[host.commands.lint]\nargv = [\"just\", \"lint\"]\n",
    );

    let loaded = Manifest::load(&path).expect("load should succeed");
    let commands = &loaded.host.commands;

    assert_eq!(commands["test"].argv, ["just", "test"]);
    assert!(!commands["test-one"].argv.is_empty(), "an unnamed command stays");
    assert!(commands.contains_key("lint"));
  }

  #[test]
  fn a_mistake_in_the_local_manifest_names_it() {
    let (_temp, path) = shared_and_local(
      "[project]\nimage = \"compostbin/base:latest\"\n",
      "[container]\ncpu = 8\n",
    );

    let error = Manifest::load(&path).expect_err("an unknown key should be refused");

    assert!(
      error.to_string().contains("compostbin.local.toml"),
      "error should name the local manifest: {error}"
    );
  }

  #[test]
  fn records_a_path_in_only_the_file_it_belongs_to() {
    let local = "[container]\ncpus = 4\n";
    let (_temp, path) = shared_and_local(FULL_MANIFEST, local);
    let entry = |local| PathEntry {
      local,
      readonly: false,
      source: "~/scratch".to_string(),
      target: None,
    };

    record_path(&path, &entry(true)).expect("record should succeed");

    assert_eq!(std::fs::read_to_string(&path).expect("manifest"), FULL_MANIFEST);
    let rewritten = std::fs::read_to_string(local_path(&path)).expect("local manifest");
    assert!(
      rewritten.contains("cpus = 4"),
      "a key set to its default still overrides the shared manifest: {rewritten}"
    );

    record_path(&path, &entry(false)).expect("record should succeed");

    let loaded = Manifest::load(&path).expect("load should succeed");
    let scratch: Vec<bool> = loaded
      .paths
      .iter()
      .filter(|entry| entry.source == "~/scratch")
      .map(|entry| entry.local)
      .collect();
    assert_eq!(scratch, [false, true]);
  }

  #[test]
  fn records_a_path_beside_host_commands() {
    let (_temp, path) = shared_and_local(&format!("[host]\nconcurrency = 2\n{HOST_MANIFEST}"), "");

    record_path(
      &path,
      &PathEntry {
        local: false,
        readonly: true,
        source: "~/scratch".to_string(),
        target: None,
      },
    )
    .expect("record should succeed");

    let loaded = Manifest::load(&path).expect("the rewritten manifest should load");
    assert_eq!(loaded.host.concurrency, 2);
    assert!(loaded.host.commands.contains_key("test"));
    assert_eq!(loaded.paths.len(), 1);
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
  fn user_config_turns_the_keychain_on() {
    assert!(!UserConfig::default().claude.keychain);

    let config: UserConfig = toml::from_str("[claude]\nkeychain = true\n").expect("config should parse");

    assert!(config.claude.keychain);
  }

  #[test]
  fn user_config_refuses_project_settings() {
    assert!(toml::from_str::<UserConfig>("[container]\ncpus = 8\n").is_err());
    assert!(toml::from_str::<UserConfig>("[claude]\nhome = \"~/home\"\n").is_err());
  }

  #[test]
  fn manifest_refuses_keychain() {
    assert!(toml::from_str::<Manifest>("[claude]\nkeychain = true\n").is_err());
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
  fn renders_only_the_image_by_default() {
    assert_eq!(
      toml::to_string(&Manifest::default()).expect("manifest should serialize"),
      "[project]\nimage = \"compostbin/base:latest\"\n"
    );
  }

  #[test]
  fn renders_a_value_that_differs_from_its_default() {
    let manifest: Manifest =
      toml::from_str("[container]\ncpus = 2\nmemory = \"4G\"\n[host]\nclipboard = true\nconcurrency = 2\n")
        .expect("manifest should parse");

    let rendered = toml::to_string(&manifest).expect("manifest should serialize");

    for line in ["cpus = 2", "memory = \"4G\"", "concurrency = 2"] {
      assert!(rendered.contains(line), "{line} should survive: {rendered}");
    }
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
