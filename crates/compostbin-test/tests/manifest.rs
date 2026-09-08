use compostbin_core::manifest::Manifest;

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
  let error = toml::from_str::<Manifest>("[project]\nimagee = \"typo\"\n").expect_err("unknown key should be rejected");

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
