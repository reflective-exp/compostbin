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

#[test]
fn parses_every_documented_key() {
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
fn applies_documented_defaults_to_an_empty_manifest() {
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
