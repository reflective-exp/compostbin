use std::process::Command;
use tempfile::TempDir;

/// `CARGO_BIN_EXE_*` is set only for tests inside the binary's own package, and
/// cargo guarantees the binary is rebuilt before they run. Locating it by hand
/// from `current_exe` would happily test a stale build.
const BINARY: &str = env!("CARGO_BIN_EXE_compostbin");

fn compostbin(project_dir: &std::path::Path, arguments: &[&str]) -> std::process::Output {
  Command::new(BINARY)
    .args(arguments)
    .current_dir(project_dir)
    .output()
    .expect("compostbin should run")
}

#[test]
fn init_writes_a_manifest_named_after_the_directory() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = temp.path().join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");

  let output = compostbin(&project_dir, &["init"]);

  assert!(
    output.status.success(),
    "init failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    std::fs::read_to_string(project_dir.join(".config/compostbin.toml")).expect("manifest should exist"),
    r#"[claude]
home = "~/.compostbin/claude-home"
seed_from_keychain = true

[container]
cpus = 4
env = []
memory = "8G"

[project]
image = "compostbin/base:latest"
name = "my-project"

[safety]
snapshot = true

[workspace]
roots = []
"#
  );
}

#[test]
fn ls_lists_the_project_and_claude_home() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = temp
    .path()
    .canonicalize()
    .expect("canonical temp")
    .join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");
  compostbin(&project_dir, &["init"]);

  let output = compostbin(&project_dir, &["ls"]);

  let listed: Vec<String> = String::from_utf8_lossy(&output.stdout)
    .lines()
    .map(|line| line.to_string())
    .collect();
  let home = std::env::var("HOME").expect("HOME");
  assert_eq!(
    listed,
    [
      project_dir.display().to_string(),
      format!("{home}/.compostbin/claude-home"),
    ]
  );
}

#[test]
fn ls_without_a_manifest_names_the_missing_file() {
  let temp = TempDir::new().expect("temp dir");

  let output = compostbin(temp.path(), &["ls"]);

  assert!(!output.status.success(), "ls should fail without a manifest");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains(".config/compostbin.toml"),
    "error should name the manifest: {stderr}"
  );
}
