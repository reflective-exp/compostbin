use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_compostbin");

fn compostbin(project_dir: &Path, arguments: &[&str]) -> Output {
  Command::new(BINARY)
    .args(arguments)
    .current_dir(project_dir)
    .output()
    .expect("compostbin should run")
}

/// An `init`ed project under a canonical temp path, so the paths it prints back
/// are comparable.
fn initialized_project(temp: &TempDir) -> PathBuf {
  let project_dir = temp
    .path()
    .canonicalize()
    .expect("canonical temp")
    .join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");
  compostbin(&project_dir, &["init"]);

  project_dir
}

/// The state directory `compostbin` keeps for a project of that name.
fn state_dir() -> PathBuf {
  let home = std::env::var("HOME").expect("HOME");

  Path::new(&home).join(".local/state/compostbin/sessions/compostbin-my-project")
}

/// A project whose `workspace.roots` names a sibling `workspace/` tree, holding
/// `inside` and next to an `outside` that no root covers.
struct Rooted {
  project_dir: PathBuf,
  manifest_path: PathBuf,
  inside: PathBuf,
  outside: PathBuf,
}

fn project_with_a_root(temp: &TempDir) -> Rooted {
  let base = temp.path().canonicalize().expect("canonical temp");
  let project_dir = base.join("my-project");
  let manifest_path = project_dir.join(".config/compostbin.toml");
  std::fs::create_dir_all(project_dir.join(".config")).expect("create project dir");
  std::fs::create_dir_all(base.join("workspace/inside")).expect("create root");
  std::fs::create_dir_all(base.join("outside")).expect("create outside dir");
  std::fs::write(
    &manifest_path,
    format!("[workspace]\nroots = [\"{}\"]\n", base.join("workspace").display()),
  )
  .expect("write manifest");

  Rooted {
    project_dir,
    manifest_path,
    inside: base.join("workspace/inside"),
    outside: base.join("outside"),
  }
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
seed_from_keychain = true

[container]
cpus = 4
env = []
memory = "8G"

[project]
image = "compostbin/base:latest"
name = "my-project"

[workspace]
roots = []
"#
  );
}

#[test]
fn ls_shows_where_each_path_lands_in_the_container() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = initialized_project(&temp);

  let output = compostbin(&project_dir, &["ls"]);

  let listed: Vec<String> = String::from_utf8_lossy(&output.stdout)
    .lines()
    .map(|line| line.to_string())
    .collect();
  assert_eq!(
    listed,
    [
      format!("{} -> /workspace/my-project (project)", project_dir.display()),
      format!(
        "{} -> /home/claude/.claude (claude home)",
        state_dir().join("claude-home").display()
      ),
    ]
  );
}

#[test]
fn clean_empties_the_spool_and_keeps_the_conversation() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = initialized_project(&temp);

  let state = state_dir();
  std::fs::create_dir_all(state.join("host/requests")).expect("create spool");
  std::fs::write(state.join("host/requests/0001.request"), "run\n").expect("write a request");
  std::fs::create_dir_all(state.join("claude-home")).expect("create claude home");

  let output = compostbin(&project_dir, &["clean"]);

  assert!(
    output.status.success(),
    "clean failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    !state.join("host/requests/0001.request").exists(),
    "what was in flight is transient"
  );
  assert!(
    state.join("host/requests").exists(),
    "the spool is a mount source: a container holding it cannot be given a new directory"
  );
  assert!(
    state.join("claude-home").exists(),
    "`clean` must not discard the conversation `--continue` resumes"
  );

  std::fs::remove_dir_all(&state).expect("remove session state");
}

#[test]
fn add_refuses_a_path_full_of_credentials() {
  let temp = TempDir::new().expect("temp dir");
  let project = project_with_a_root(&temp);
  let home = std::env::var("HOME").expect("HOME");
  let ssh = Path::new(&home).join(".ssh");
  if !ssh.is_dir() {
    return;
  }

  let output = compostbin(&project.project_dir, &["add", &ssh.display().to_string()]);

  assert!(!output.status.success(), "add should refuse ~/.ssh");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("--force"),
    "the refusal should name the override: {stderr}"
  );
  let manifest = std::fs::read_to_string(&project.manifest_path).expect("manifest");
  assert!(
    !manifest.contains("[[paths]]"),
    "a refused path must not be recorded: {manifest}"
  );
}

#[test]
fn add_inside_a_root_records_no_path_entry() {
  let temp = TempDir::new().expect("temp dir");
  let project = project_with_a_root(&temp);
  let mut before = std::fs::read_to_string(&project.manifest_path).expect("manifest");
  before.insert_str(0, "# the user's own comment\n");
  std::fs::write(&project.manifest_path, &before).expect("write manifest");

  let output = compostbin(&project.project_dir, &["add", &project.inside.display().to_string()]);

  assert!(
    output.status.success(),
    "add failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    String::from_utf8_lossy(&output.stdout).contains("already mounted"),
    "stdout: {}",
    String::from_utf8_lossy(&output.stdout)
  );
  let manifest = std::fs::read_to_string(&project.manifest_path).expect("manifest");
  assert_eq!(manifest, before, "an in-root path must leave the manifest untouched");
}

#[test]
fn add_outside_every_root_records_a_path_and_says_how_to_mount_it() {
  let temp = TempDir::new().expect("temp dir");
  let project = project_with_a_root(&temp);

  let output = compostbin(
    &project.project_dir,
    &["add", &project.outside.display().to_string(), "--readonly"],
  );

  assert!(
    output.status.success(),
    "add failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    String::from_utf8_lossy(&output.stdout).contains("compostbin run -- --continue"),
    "stdout must say how to make the path visible: {}",
    String::from_utf8_lossy(&output.stdout)
  );
  let manifest = std::fs::read_to_string(&project.manifest_path).expect("manifest");
  assert!(
    manifest.contains(&format!("source = \"{}\"", project.outside.display())),
    "manifest: {manifest}"
  );
  assert!(manifest.contains("readonly = true"), "manifest: {manifest}");
}

#[test]
fn add_local_records_the_path_beside_the_committed_manifest() {
  let temp = TempDir::new().expect("temp dir");
  let project = project_with_a_root(&temp);
  let committed = std::fs::read_to_string(&project.manifest_path).expect("manifest");

  let output = compostbin(
    &project.project_dir,
    &["add", &project.outside.display().to_string(), "--local"],
  );

  assert!(
    output.status.success(),
    "add failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(
    std::fs::read_to_string(&project.manifest_path).expect("manifest"),
    committed,
    "--local must leave the committed manifest untouched"
  );
  let local =
    std::fs::read_to_string(project.project_dir.join(".config/compostbin.local.toml")).expect("local manifest");
  assert!(
    local.contains(&format!("source = \"{}\"", project.outside.display())),
    "local manifest: {local}"
  );

  // And the session mounts it, saying where it came from.
  let listed = compostbin(&project.project_dir, &["ls"]);
  let stdout = String::from_utf8_lossy(&listed.stdout);
  assert!(
    stdout.contains("local") && stdout.contains(&project.outside.display().to_string()),
    "ls should list the local path as local: {stdout}"
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
