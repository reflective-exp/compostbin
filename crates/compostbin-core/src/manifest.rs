use serde::Deserialize;

pub const DEFAULT_CLAUDE_HOME: &str = "~/.compostbin/claude-home";
pub const DEFAULT_CONTAINER_CPUS: u32 = 4;
pub const DEFAULT_CONTAINER_MEMORY: &str = "8G";
pub const DEFAULT_IMAGE: &str = "compostbin/base:latest";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Manifest {
  pub claude: ClaudeConfig,
  pub container: ContainerConfig,
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  pub safety: SafetyConfig,
  pub workspace: WorkspaceConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Deserialize)]
#[serde(default)]
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

#[derive(Debug, Deserialize)]
pub struct PathEntry {
  #[serde(default)]
  pub readonly: bool,
  pub source: String,
  #[serde(default)]
  pub target: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
  pub image: String,
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

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
  pub snapshot: bool,
}

impl Default for SafetyConfig {
  fn default() -> Self {
    Self { snapshot: true }
  }
}

/// Roots default to empty: mounting a workspace tree read-write is opt-in.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
  pub roots: Vec<String>,
}
