use crate::error::{ManifestError, PathError};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::Path;

/// Session state, keyed by container name: Claude's home, which persists so
/// `--continue` works, beside the spool, which `clean` removes.
pub const SESSIONS_DIR: &str = "~/.local/state/compostbin/sessions";
pub const DEFAULT_CONTAINER_CPUS: u32 = 4;
pub const DEFAULT_CONTAINER_MEMORY: &str = "8G";
pub const DEFAULT_HOST_CONCURRENCY: usize = 8;
pub const DEFAULT_IMAGE: &str = "compostbin/base:latest";
/// Checked in beside the project it configures.
pub const MANIFEST_RELATIVE_PATH: &str = ".config/compostbin.toml";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
  pub claude: ClaudeConfig,
  pub container: ContainerConfig,
  /// Skipped when empty, so a manifest that declares no host commands renders
  /// no table.
  #[serde(skip_serializing_if = "HostConfig::is_empty")]
  pub host: HostConfig,
  /// Per-project additions to the base image, skipped when empty so a project
  /// that needs nothing renders no table.
  #[serde(skip_serializing_if = "ImageConfig::is_empty")]
  pub image: ImageConfig,
  #[serde(
    serialize_with = "PathEntry::serialize_sorted_by_source",
    skip_serializing_if = "Vec::is_empty"
  )]
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  pub workspace: WorkspaceConfig,
}

impl Manifest {
  pub fn load(path: &Path) -> Result<Self, ManifestError> {
    let text = std::fs::read_to_string(path).map_err(|source| ManifestError::Io(PathError::new(path, source)))?;

    toml::from_str(&text).map_err(|source| ManifestError::Parse {
      path: path.to_path_buf(),
      source,
    })
  }

  /// Creates the parent directory, so `init` works in a project with no `.config`.
  pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
    let rendered = toml::to_string(self).map_err(|source| ManifestError::Render {
      path: path.to_path_buf(),
      source,
    })?;

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).map_err(|source| ManifestError::Io(PathError::new(parent, source)))?;
    }

    std::fs::write(path, rendered).map_err(|source| ManifestError::Io(PathError::new(path, source)))
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeConfig {
  /// Overrides the per-session default under `SESSIONS_DIR`. Normally unset:
  /// sharing one home between projects would make `--continue` resume whichever
  /// project spoke last.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub home: Option<String>,
  pub seed_from_keychain: bool,
  /// Extra `~/.claude` entries — files or whole directories — this project wants
  /// beyond the host settings every session already gets. Skipped when empty, so
  /// a manifest says nothing about sharing until it has something to add.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub shared: Vec<String>,
}

impl Default for ClaudeConfig {
  fn default() -> Self {
    Self {
      home: None,
      seed_from_keychain: true,
      shared: Vec::new(),
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

/// Commands the guest may ask the host to run, keyed by the name it sends. A map
/// because the name is a lookup key, and a `BTreeMap` because rendering
/// alphabetically keeps diffs deterministic.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
  /// How many may run at once: subagents call `compostbin-host` independently,
  /// and the bound stops a guest looping on submissions from spawning unlimited
  /// work.
  ///
  /// Declared before `commands`, against this table's alphabetical order, and it
  /// must stay there: TOML cannot express a bare value after a table.
  pub concurrency: usize,
  pub commands: BTreeMap<String, HostCommand>,
}

impl Default for HostConfig {
  fn default() -> Self {
    Self {
      commands: BTreeMap::new(),
      concurrency: DEFAULT_HOST_CONCURRENCY,
    }
  }
}

impl HostConfig {
  /// No commands, no channel: the spool is neither created nor mounted, so a
  /// project that has not opted in has no guest-to-host path.
  pub fn is_empty(&self) -> bool {
    self.commands.is_empty()
  }
}

/// `argv` lives only on the host; the guest sends the key, never a command line.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommand {
  /// Off by default, so a command is exact unless deliberately widened.
  #[serde(default)]
  pub arguments: bool,
  pub argv: Vec<String>,
  /// Run under a pty, so colour, progress and prompts work. A terminal is one
  /// device, so this merges stdout and stderr — hence off by default, keeping
  /// them separate for anything read by a machine.
  #[serde(default)]
  pub tty: bool,
}

/// Per-project image additions, for what belongs to one project rather than
/// every project — direnv, say. Non-empty means the session runs a derived image
/// built `FROM` the base; empty means it runs the base image itself.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
  /// `apt-get install`ed as root, before `run`.
  pub packages: Vec<String>,
  /// Shell lines run as the `claude` user, so a command writing to `~` lands in
  /// the home the session actually uses.
  pub run: Vec<String>,
}

impl ImageConfig {
  pub fn is_empty(&self) -> bool {
    self.packages.is_empty() && self.run.is_empty()
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathEntry {
  #[serde(default)]
  pub readonly: bool,
  pub source: String,
  /// An absolute *guest* path, for the rare case where something must appear at
  /// a fixed location. The default, `/workspace/<basename>`, is what keeps host
  /// paths out of the container.
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

/// Roots default to empty: mounting a workspace tree read-write is opt-in.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
  pub roots: Vec<String>,
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

[workspace]
roots = ["~/workspace"]

[[paths]]
readonly = true
source   = "~/.cargo/registry"

[[paths]]
source = "~/code/vendor/libfoo"
target = "~/code/vendor/libfoo"

[claude]
seed_from_keychain = true
shared             = ["agents"]

[image]
packages = ["direnv"]
run      = ["echo 'eval \"$(direnv hook bash)\"' >> ~/.bashrc"]
"#;

  const EXPECTED_RENDERING: &str = r#"[claude]
seed_from_keychain = true
shared = ["agents"]

[container]
cpus = 4
env = ["ANTHROPIC_API_KEY", "GITHUB_TOKEN"]
memory = "8G"

[image]
packages = ["direnv"]
run = ["""echo 'eval "$(direnv hook bash)"' >> ~/.bashrc"""]

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

  /// TOML cannot express a bare value after a table, so `concurrency` has to
  /// serialise before `commands`; reversing that order breaks this save.
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
    assert_eq!(manifest.container.memory, "8G");

    assert_eq!(manifest.workspace.roots, ["~/workspace"]);

    assert_eq!(manifest.paths[0].readonly, true);
    assert_eq!(manifest.paths[0].source, "~/.cargo/registry");
    assert_eq!(manifest.paths[0].target, None);
    assert_eq!(manifest.paths[1].readonly, false);
    assert_eq!(manifest.paths[1].source, "~/code/vendor/libfoo");
    assert_eq!(manifest.paths[1].target.as_deref(), Some("~/code/vendor/libfoo"));
    assert_eq!(manifest.paths.len(), 2);

    assert_eq!(manifest.claude.home, None);
    assert_eq!(manifest.claude.seed_from_keychain, true);
    assert_eq!(manifest.claude.shared, ["agents"]);

    assert_eq!(manifest.image.packages, ["direnv"]);
    assert_eq!(manifest.image.run.len(), 1);
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
  fn applies_defaults() {
    let manifest: Manifest = toml::from_str("").expect("empty manifest should parse");

    assert_eq!(manifest.project.image, "compostbin/base:latest");
    assert_eq!(manifest.project.name, None);

    assert_eq!(manifest.container.cpus, 4);
    assert_eq!(manifest.container.env, [] as [String; 0]);
    assert_eq!(manifest.container.memory, "8G");

    assert_eq!(manifest.workspace.roots, [] as [String; 0]);
    assert_eq!(manifest.paths.len(), 0);

    assert_eq!(
      manifest.claude.home, None,
      "an unset home is what makes the session's home per project"
    );
    assert_eq!(manifest.claude.seed_from_keychain, true);
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
