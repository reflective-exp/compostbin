use serde::{Deserialize, Serialize, Serializer};

pub const DEFAULT_CLAUDE_HOME: &str = "~/.compostbin/claude-home";
pub const DEFAULT_CONTAINER_CPUS: u32 = 4;
pub const DEFAULT_CONTAINER_MEMORY: &str = "8G";
pub const DEFAULT_IMAGE: &str = "compostbin/base:latest";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
  pub claude: ClaudeConfig,
  pub container: ContainerConfig,
  #[serde(serialize_with = "PathEntry::serialize_sorted_by_source")]
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  pub safety: SafetyConfig,
  pub workspace: WorkspaceConfig,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
  pub home: String,
  pub seed_from_keychain: bool,
}

impl Default for ClaudeConfig {
  fn default() -> Self {
    Self {
      home: DEFAULT_CLAUDE_HOME.to_string(),
      seed_from_keychain: true,
    }
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerConfig {
  pub cpus: u32,
  pub env: Vec<String>,
  pub memory: String,
}

impl Default for ContainerConfig {
  fn default() -> Self {
    Self {
      cpus: DEFAULT_CONTAINER_CPUS,
      env: Vec::new(),
      memory: DEFAULT_CONTAINER_MEMORY.to_string(),
    }
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
  #[serde(default)]
  pub readonly: bool,
  pub source: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub target: Option<String>,
}

impl PathEntry {
  pub fn sort_by_source(entries: &[PathEntry]) -> Vec<&PathEntry> {
    let mut sorted: Vec<&PathEntry> = entries.iter().collect();
    sorted.sort_by(|left, right| left.source.cmp(&right.source));
    sorted
  }

  fn serialize_sorted_by_source<S: Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    Self::sort_by_source(entries).serialize(serializer)
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SafetyConfig {
  pub snapshot: bool,
}

impl Default for SafetyConfig {
  fn default() -> Self {
    Self { snapshot: true }
  }
}

/// Roots default to empty: mounting a workspace tree read-write is opt-in.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
  pub roots: Vec<String>,
}
