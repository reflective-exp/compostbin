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
fn ls_shows_where_each_path_lands_in_the_container() {
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
      format!("{} -> /workspace/my-project (project)", project_dir.display()),
      format!(
        "{home}/.local/state/compostbin/sessions/compostbin-my-project/claude-home -> /home/claude/.claude (claude home)"
      ),
    ]
  );
}

#[test]
fn clean_removes_the_spool_and_keeps_the_conversation() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = temp
    .path()
    .canonicalize()
    .expect("canonical temp")
    .join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");
  compostbin(&project_dir, &["init"]);

  let home = std::env::var("HOME").expect("HOME");
  let state = std::path::Path::new(&home).join(".local/state/compostbin/sessions/compostbin-my-project");
  std::fs::create_dir_all(state.join("host/requests")).expect("create spool");
  std::fs::create_dir_all(state.join("claude-home")).expect("create claude home");

  let output = compostbin(&project_dir, &["clean"]);

  assert!(
    output.status.success(),
    "clean failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!state.join("host").exists(), "the spool is transient");
  assert!(
    state.join("claude-home").exists(),
    "`clean` must not discard the conversation `--continue` resumes"
  );

  std::fs::remove_dir_all(&state).expect("remove session state");
}

#[test]
fn add_refuses_a_path_full_of_credentials() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = project_with_a_root(&temp);
  let home = std::env::var("HOME").expect("HOME");
  let ssh = std::path::Path::new(&home).join(".ssh");
  if !ssh.is_dir() {
    return;
  }

  let output = compostbin(&project_dir, &["add", &ssh.display().to_string()]);

  assert!(!output.status.success(), "add should refuse ~/.ssh");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains("--force"),
    "the refusal should name the override: {stderr}"
  );
  let manifest = std::fs::read_to_string(project_dir.join(".config/compostbin.toml")).expect("manifest");
  assert!(
    !manifest.contains("[[paths]]"),
    "a refused path must not be recorded: {manifest}"
  );
}

/// A project with `workspace.roots` pointing at a sibling `workspace/` tree.
fn project_with_a_root(temp: &TempDir) -> std::path::PathBuf {
  let base = temp.path().canonicalize().expect("canonical temp");
  let project_dir = base.join("my-project");
  std::fs::create_dir_all(project_dir.join(".config")).expect("create project dir");
  std::fs::create_dir_all(base.join("workspace/inside")).expect("create root");
  std::fs::create_dir_all(base.join("outside")).expect("create outside dir");
  std::fs::write(
    project_dir.join(".config/compostbin.toml"),
    format!("[workspace]\nroots = [\"{}\"]\n", base.join("workspace").display()),
  )
  .expect("write manifest");

  project_dir
}

#[test]
fn add_inside_a_root_records_no_path_entry() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = project_with_a_root(&temp);
  let inside = temp
    .path()
    .canonicalize()
    .expect("canonical temp")
    .join("workspace/inside");

  let output = compostbin(&project_dir, &["add", &inside.display().to_string()]);

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
  let manifest = std::fs::read_to_string(project_dir.join(".config/compostbin.toml")).expect("manifest");
  assert!(
    !manifest.contains("[[paths]]"),
    "an in-root path must not be recorded: {manifest}"
  );
}

#[test]
fn add_outside_every_root_records_a_path_without_restarting() {
  let temp = TempDir::new().expect("temp dir");
  let project_dir = project_with_a_root(&temp);
  let outside = temp
    .path()
    .canonicalize()
    .expect("canonical temp")
    .join("outside");

  let output = compostbin(&project_dir, &["add", &outside.display().to_string(), "--readonly"]);

  assert!(
    output.status.success(),
    "add failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(
    String::from_utf8_lossy(&output.stdout).contains("--restart"),
    "stdout must say how to make the path visible: {}",
    String::from_utf8_lossy(&output.stdout)
  );
  let manifest = std::fs::read_to_string(project_dir.join(".config/compostbin.toml")).expect("manifest");
  assert!(
    manifest.contains(&format!("source = \"{}\"", outside.display())),
    "manifest: {manifest}"
  );
  assert!(manifest.contains("readonly = true"), "manifest: {manifest}");
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
