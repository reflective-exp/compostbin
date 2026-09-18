use crate::error::{At, ManifestError};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Session state, keyed by container name: Claude's home, which persists so
/// `--continue` works, beside the spool, which `clean` removes.
pub const SESSIONS_DIR: &str = "~/.local/state/compostbin/sessions";
pub const DEFAULT_CONTAINER_CPUS: u32 = 4;
pub const DEFAULT_CONTAINER_MEMORY: Memory = Memory::gibibytes(8);
pub const DEFAULT_HOST_CONCURRENCY: usize = 8;
pub const DEFAULT_IMAGE: &str = "compostbin/base:latest";
/// Checked in beside the project it configures.
pub const MANIFEST_RELATIVE_PATH: &str = ".config/compostbin.toml";
/// Beside the manifest, and deliberately not checked in: one developer's own
/// `[[paths]]`, which the rest of the project has no reason to mount.
pub const LOCAL_MANIFEST_RELATIVE_PATH: &str = ".config/compostbin.local.toml";
/// The host command `[host] clipboard` serves, and what the guest's `pbcopy`,
/// `xclip`, `xsel` and `wl-copy` send.
pub const CLIPBOARD_COMMAND: &str = "clipboard";
const CLIPBOARD_ARGV: &[&str] = &["pbcopy"];

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
  /// Both files' entries, each knowing which one it came from; rendering keeps
  /// only the ones that belong to the file being written.
  #[serde(
    serialize_with = "PathEntry::serialize_shared",
    skip_serializing_if = "PathEntry::none_shared"
  )]
  pub paths: Vec<PathEntry>,
  pub project: ProjectConfig,
  pub workspace: WorkspaceConfig,
}

impl Manifest {
  /// The committed manifest, plus the local one beside it when there is one.
  pub fn load(path: &Path) -> Result<Self, ManifestError> {
    let mut manifest = Self::parse(path)?;
    manifest.paths.extend(Self::parse_local(&local_path(path))?);

    Ok(manifest)
  }

  fn parse(path: &Path) -> Result<Self, ManifestError> {
    let text = std::fs::read_to_string(path).at(path)?;

    toml::from_str(&text).map_err(|source| ManifestError::Parse {
      path: path.to_path_buf(),
      source,
    })
  }

  /// Missing is the normal case — most checkouts have no local manifest — so
  /// only a file that exists and does not parse is an error.
  fn parse_local(path: &Path) -> Result<Vec<PathEntry>, ManifestError> {
    let text = match std::fs::read_to_string(path).at(path) {
      Ok(text) => text,
      Err(error) if error.is_not_found() => return Ok(Vec::new()),
      Err(error) => return Err(error.into()),
    };

    let local: LocalManifest = toml::from_str(&text).map_err(|source| ManifestError::Parse {
      path: path.to_path_buf(),
      source,
    })?;

    Ok(
      local
        .paths
        .into_iter()
        .map(|entry| PathEntry { local: true, ..entry })
        .collect(),
    )
  }

  /// Writes the local `[[paths]]` beside the manifest, leaving the committed
  /// file alone. Removes the file when nothing local is left, so an emptied
  /// overlay does not linger as an empty one.
  pub fn save_local(&self, manifest_path: &Path) -> Result<(), ManifestError> {
    let path = local_path(manifest_path);
    let local = LocalManifest {
      paths: self
        .paths
        .iter()
        .filter(|entry| entry.local)
        .cloned()
        .collect(),
    };

    if local.paths.is_empty() {
      return match std::fs::remove_file(&path).at(&path) {
        Err(error) if !error.is_not_found() => Err(error.into()),
        _ => Ok(()),
      };
    }

    let rendered = toml::to_string(&local).map_err(|source| ManifestError::Render {
      path: path.clone(),
      source,
    })?;

    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).at(parent)?;
    }

    Ok(std::fs::write(&path, rendered).at(&path)?)
  }

  /// Creates the parent directory, so `init` works in a project with no `.config`.
  pub fn save(&self, path: &Path) -> Result<(), ManifestError> {
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

/// The uncommitted manifest beside a committed one: `[[paths]]` and nothing
/// else, since everything else in a manifest describes the project rather than
/// the developer.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalManifest {
  #[serde(serialize_with = "PathEntry::serialize_sorted_by_source")]
  pub paths: Vec<PathEntry>,
}

/// `…/compostbin.toml` becomes `…/compostbin.local.toml`, so a manifest found
/// anywhere — a test's temporary directory included — has its overlay beside it.
fn local_path(manifest_path: &Path) -> PathBuf {
  manifest_path.with_extension("local.toml")
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
  pub memory: Memory,
}

impl Default for ContainerConfig {
  fn default() -> Self {
    Self {
      cpus: DEFAULT_CONTAINER_CPUS,
      env: Vec::new(),
      memory: DEFAULT_CONTAINER_MEMORY,
    }
  }
}

/// A size in bytes, written the way a manifest writes it: `8G`, `512M`,
/// `1024K`, or a bare number of bytes. Parsed as the manifest loads, so a typo
/// is an error naming the file rather than a session quietly given some other
/// size.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Memory(u64);

/// Largest first, which is the order `Display` wants them in.
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
  type Err = String;

  fn from_str(text: &str) -> Result<Self, String> {
    let invalid = || format!("\"{text}\" is not a memory size: expected a number, optionally followed by G, M, or K");
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
  type Error = String;

  fn try_from(text: String) -> Result<Self, String> {
    text.parse()
  }
}

impl From<Memory> for String {
  fn from(memory: Memory) -> Self {
    memory.to_string()
  }
}

/// In the largest unit that divides it exactly, so `8G` is written back as it
/// was read.
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

/// Commands the guest may ask the host to run, keyed by the name it sends. A map
/// because the name is a lookup key, and a `BTreeMap` because rendering
/// alphabetically keeps diffs deterministic.
#[derive(Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
  /// Lets the guest's clipboard tools write to the host's pasteboard, by serving
  /// `clipboard` as though it were declared. Off by default: whatever the guest
  /// copies, the user may later paste into a host terminal. Write-only, so
  /// nothing on the host clipboard reaches the guest.
  ///
  /// A bare value, so before `commands` for the same reason as `concurrency`.
  #[serde(skip_serializing_if = "std::ops::Not::not")]
  pub clipboard: bool,
  /// How many may run at once: subagents call `compostbin-host` independently,
  /// and the bound stops a guest looping on submissions from spawning unlimited
  /// work.
  ///
  /// Declared before `commands`, against this table's alphabetical order, and it
  /// must stay there: TOML cannot express a bare value after a table.
  pub concurrency: usize,
  /// Host loopback ports the guest reaches at its own `localhost`, each relayed
  /// through a unix socket carried into this container alone. A bare value, so
  /// before `commands` for the same reason as `concurrency`.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub ports: Vec<u16>,
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
  /// No commands, no channel: the spool is neither created nor mounted, so a
  /// project that has not opted in has no way to run anything on the host.
  pub fn has_commands(&self) -> bool {
    self.clipboard || !self.commands.is_empty()
  }

  /// What the agent serves: `commands`, plus `clipboard` when it is on. A
  /// declared `clipboard` wins, so a project can point it elsewhere.
  pub fn served_commands(&self) -> BTreeMap<String, HostCommand> {
    let mut served = self.commands.clone();

    if self.clipboard {
      served
        .entry(CLIPBOARD_COMMAND.to_string())
        .or_insert_with(|| HostCommand {
          arguments: false,
          argv: CLIPBOARD_ARGV.iter().copied().map(str::to_string).collect(),
          deny: Vec::new(),
          tty: false,
        });
    }

    served
  }

  /// No ports, no listener: nothing binds on the host.
  pub fn has_ports(&self) -> bool {
    !self.ports.is_empty()
  }

  /// Neither commands nor ports: the guest has no path to the host at all.
  pub fn is_empty(&self) -> bool {
    !self.has_commands() && !self.has_ports()
  }
}

/// `argv` lives only on the host; the guest sends the key, never a command line.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCommand {
  /// Off by default, so a command is exact unless deliberately widened.
  #[serde(default)]
  pub arguments: bool,
  pub argv: Vec<String>,
  /// Guest arguments refused even when `arguments` is on — flags that point the
  /// command at other code or configuration, which only the project knows for
  /// its toolchain. `--config` also refuses `--config=x`; a single-letter `-Z`
  /// also refuses the joined `-Zx`.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub deny: Vec<String>,
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
  /// `apt-get install`ed as root, before `run_as_root` and `run`.
  pub packages: Vec<String>,
  /// Shell lines run as the `claude` user, so a command writing to `~` lands in
  /// the home the session actually uses.
  pub run: Vec<String>,
  /// Shell lines run as root, after `packages` and before the image drops to
  /// `claude` — for what only root can do, like writing under `/etc`. Anything
  /// touching the session's home belongs in `run`.
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
  /// Which file the entry came from, never written: an entry belongs to the
  /// local manifest or the committed one, and the file it is written back to is
  /// what says so.
  #[serde(skip)]
  pub local: bool,
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
  fn sort_by_source(mut entries: Vec<&PathEntry>) -> Vec<&PathEntry> {
    entries.sort_by(|left, right| left.source.cmp(&right.source));
    entries
  }

  fn serialize_sorted_by_source<S: Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    Self::sort_by_source(entries.iter().collect()).serialize(serializer)
  }

  /// The committed manifest's own entries. Local ones are dropped rather than
  /// rendered, so saving a manifest loaded with an overlay writes back what it
  /// read.
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
packages    = ["direnv"]
run         = ["echo 'eval \"$(direnv hook bash)\"' >> ~/.bashrc"]
run_as_root = ["install -d -o claude /opt/vendor"]
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

  /// Ports alone are a path out to the host, but not a reason for a spool.
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

  /// Like `concurrency`, `ports` is a bare value and has to render before the
  /// `commands` tables; this save fails if it does not.
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

  /// The clipboard alone is a reason for the spool: it is served through it.
  #[test]
  fn clipboard_alone_opens_the_spool() {
    let manifest: Manifest = toml::from_str("[host]\nclipboard = true\n").expect("should parse");

    assert!(manifest.host.has_commands());
    assert_eq!(manifest.host.served_commands()[CLIPBOARD_COMMAND].argv, ["pbcopy"]);

    let rendered = toml::to_string(&manifest).expect("should serialize");
    assert!(rendered.contains("[host]\nclipboard = true\n"), "{rendered}");
  }

  #[test]
  fn serves_no_clipboard_unless_asked() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    assert!(
      !manifest
        .host
        .served_commands()
        .contains_key(CLIPBOARD_COMMAND)
    );
    assert!(
      !toml::to_string(&manifest)
        .expect("should serialize")
        .contains("clipboard")
    );
  }

  #[test]
  fn a_declared_clipboard_command_wins() {
    let manifest: Manifest =
      toml::from_str("[host]\nclipboard = true\n[host.commands.clipboard]\nargv = [\"tee\", \"/tmp/copied\"]\n")
        .expect("should parse");

    assert_eq!(
      manifest.host.served_commands()[CLIPBOARD_COMMAND].argv,
      ["tee", "/tmp/copied"]
    );
  }

  #[test]
  fn omits_empty_ports() {
    let manifest: Manifest = toml::from_str(HOST_MANIFEST).expect("should parse");

    let rendered = toml::to_string(&manifest).expect("should serialize");

    assert!(!rendered.contains("ports"), "{rendered}");
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

  /// The whole point of the overlay: what one developer mounts is loaded beside
  /// the project's own paths, and is not written back into the shared file.
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

  /// A manifest whose only paths are local must render no `[[paths]]` at all,
  /// rather than an empty array of tables.
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

    // Emptied, the file goes: nothing local is left for it to record.
    manifest.paths.retain(|entry| !entry.local);
    manifest.save_local(&path).expect("save should succeed");
    assert!(!local_path(&path).exists());
  }

  #[test]
  fn a_local_manifest_takes_paths_only() {
    let temp = TempDir::new().expect("temp dir");
    let path = temp.path().join(MANIFEST_RELATIVE_PATH);
    Manifest::default()
      .save(&path)
      .expect("save should succeed");
    std::fs::write(local_path(&path), "[container]\ncpus = 8\n").expect("local manifest should write");

    let error = Manifest::load(&path).expect_err("anything but paths should be refused");

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
    assert_eq!(manifest.image.run_as_root, ["install -d -o claude /opt/vendor"]);
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

  /// Caught as the manifest loads, not left for the engine to replace with a
  /// size nobody asked for.
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
