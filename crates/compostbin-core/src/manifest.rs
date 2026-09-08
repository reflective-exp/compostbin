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

#[cfg(test)]
mod tests {
  use super::*;

  const FULL_MANIFEST: &str = r#"
[project]
image = "compostbin/base:latest"
name  = "compostbin"

[container]
cpus   = 4
env    = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
memory = "8G"

[workspace]
roots = ["~/workspace"]

[[paths]]
readonly = true
source   = "~/.cargo/registry"

[[paths]]
source = "~/code/vendor/libfoo"
target = "~/code/vendor/libfoo"

[claude]
home               = "~/.compostbin/claude-home"
seed_from_keychain = true

[safety]
snapshot = true
"#;

  const EXPECTED_RENDERING: &str = r#"[claude]
home = "~/.compostbin/claude-home"
seed_from_keychain = true

[container]
cpus = 4
env = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
memory = "8G"

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

[safety]
snapshot = true

[workspace]
roots = ["~/workspace"]
"#;

  #[test]
  fn parses_all_keys() {
    let manifest: Manifest = toml::from_str(FULL_MANIFEST).expect("manifest should parse");

    assert_eq!(manifest.project.image, "compostbin/base:latest");
    assert_eq!(manifest.project.name.as_deref(), Some("compostbin"));

    assert_eq!(manifest.container.cpus, 4);
    assert_eq!(manifest.container.env, ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]);
    assert_eq!(manifest.container.memory, "8G");

    assert_eq!(manifest.workspace.roots, ["~/workspace"]);

    assert_eq!(manifest.paths[0].readonly, true);
    assert_eq!(manifest.paths[0].source, "~/.cargo/registry");
    assert_eq!(manifest.paths[0].target, None);
    assert_eq!(manifest.paths[1].readonly, false);
    assert_eq!(manifest.paths[1].source, "~/code/vendor/libfoo");
    assert_eq!(manifest.paths[1].target.as_deref(), Some("~/code/vendor/libfoo"));
    assert_eq!(manifest.paths.len(), 2);

    assert_eq!(manifest.claude.home, "~/.compostbin/claude-home");
    assert_eq!(manifest.claude.seed_from_keychain, true);

    assert_eq!(manifest.safety.snapshot, true);
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
  fn applies_defaults() {
    let manifest: Manifest = toml::from_str("").expect("empty manifest should parse");

    assert_eq!(manifest.project.image, "compostbin/base:latest");
    assert_eq!(manifest.project.name, None);

    assert_eq!(manifest.container.cpus, 4);
    assert_eq!(manifest.container.env, [] as [String; 0]);
    assert_eq!(manifest.container.memory, "8G");

    assert_eq!(manifest.workspace.roots, [] as [String; 0]);
    assert_eq!(manifest.paths.len(), 0);

    assert_eq!(manifest.claude.home, "~/.compostbin/claude-home");
    assert_eq!(manifest.claude.seed_from_keychain, true);

    assert_eq!(manifest.safety.snapshot, true);
  }
}
