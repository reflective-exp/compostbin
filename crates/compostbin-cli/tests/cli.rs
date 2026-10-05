use compostbin_core::host::Spool;
use compostbin_core::session::Session;
use compostbin_core::workspace::paths::PathResolver;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_compostbin");

/// Runs `compostbin` in `dir` on a throwaway `home`, so no test reads or writes
/// the state of whoever runs it. Stdin is closed.
fn compostbin(home: &Path, dir: &Path, arguments: &[&str]) -> Output {
  Command::new(BINARY)
    .args(arguments)
    .current_dir(dir)
    .env("HOME", home)
    .output()
    .expect("compostbin should run")
}

/// The temp directory's canonical path, so the paths `compostbin` prints back
/// are comparable.
fn home(temp: &TempDir) -> PathBuf {
  temp.path().canonicalize().expect("canonical temp")
}

/// An `init`ed project in `home`.
fn initialized_project(home: &Path) -> PathBuf {
  let project_dir = home.join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");
  compostbin(home, &project_dir, &["init"]);

  project_dir
}

/// The session `compostbin` runs for the project in `project_dir`, under `home`.
fn session(home: &Path, project_dir: &Path) -> Session {
  Session::load(None, PathResolver::new(project_dir, home), project_dir).expect("the manifest should load")
}

/// A project whose `workspace.roots` names a sibling `workspace/` tree, holding
/// `inside` and next to an `outside` that no root covers.
struct Rooted {
  home: PathBuf,
  project_dir: PathBuf,
  manifest_path: PathBuf,
  inside: PathBuf,
  outside: PathBuf,
}

fn project_with_a_root(temp: &TempDir) -> Rooted {
  let base = home(temp);
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
    home: base,
  }
}

#[test]
fn init_writes_a_manifest_named_after_the_directory() {
  let temp = TempDir::new().expect("temp dir");
  let home = home(&temp);
  let project_dir = home.join("my-project");
  std::fs::create_dir(&project_dir).expect("create project dir");

  let output = compostbin(&home, &project_dir, &["init"]);

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
  let home = home(&temp);
  let project_dir = initialized_project(&home);

  let output = compostbin(&home, &project_dir, &["ls"]);

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
        session(&home, &project_dir).claude_home().display()
      ),
    ]
  );
}

#[test]
fn clean_empties_the_spool_and_keeps_the_conversation() {
  let temp = TempDir::new().expect("temp dir");
  let home = home(&temp);
  let project_dir = initialized_project(&home);

  let session = session(&home, &project_dir);
  let requests = Spool::new(session.host_spool()).requests();
  let request = requests.join("0001.request");
  let claude_home = session.claude_home();
  std::fs::create_dir_all(&requests).expect("create spool");
  std::fs::write(&request, "run\n").expect("write a request");
  std::fs::create_dir_all(&claude_home).expect("create claude home");

  let output = compostbin(&home, &project_dir, &["clean"]);

  assert!(
    output.status.success(),
    "clean failed: {}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!request.exists(), "what was in flight is transient");
  assert!(
    requests.exists(),
    "the spool is a mount source: a container holding it cannot be given a new directory"
  );
  assert!(
    claude_home.exists(),
    "`clean` must not discard the conversation `--continue` resumes"
  );
}

#[test]
fn add_refuses_a_path_full_of_credentials() {
  let temp = TempDir::new().expect("temp dir");
  let project = project_with_a_root(&temp);
  let ssh = project.home.join(".ssh");
  std::fs::create_dir(&ssh).expect("create .ssh");
  std::fs::write(ssh.join("id_ed25519"), "a private key\n").expect("write a key");

  let output = compostbin(
    &project.home,
    &project.project_dir,
    &["add", &ssh.display().to_string()],
  );

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

  let output = compostbin(
    &project.home,
    &project.project_dir,
    &["add", &project.inside.display().to_string()],
  );

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
    &project.home,
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
    &project.home,
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
  let listed = compostbin(&project.home, &project.project_dir, &["ls"]);
  let stdout = String::from_utf8_lossy(&listed.stdout);
  assert!(
    stdout.contains("local") && stdout.contains(&project.outside.display().to_string()),
    "ls should list the local path as local: {stdout}"
  );
}

#[test]
fn ls_without_a_manifest_names_the_missing_file() {
  let temp = TempDir::new().expect("temp dir");

  let home = home(&temp);

  let output = compostbin(&home, &home, &["ls"]);

  assert!(!output.status.success(), "ls should fail without a manifest");
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(
    stderr.contains(".config/compostbin.toml"),
    "error should name the manifest: {stderr}"
  );
}

#[test]
fn install_without_a_terminal_lists_changes_and_asks_for_yes() {
  let temp = TempDir::new().expect("temp dir");

  let output = compostbin(temp.path(), temp.path(), &["install"]);

  assert_eq!(output.status.code(), Some(1));
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert!(
    stdout.contains("create ") && stdout.contains(".claude/skills/compostbin-manifest/SKILL.md"),
    "{stdout}"
  );
  assert!(stdout.contains(".config/compostbin/profiles/"), "{stdout}");
  assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
  assert!(!temp.path().join(".claude").exists(), "nothing is written unasked");
  assert!(!temp.path().join(".config").exists(), "nothing is written unasked");
}

#[test]
fn install_yes_writes_the_skill_and_then_is_up_to_date() {
  let temp = TempDir::new().expect("temp dir");

  let output = compostbin(temp.path(), temp.path(), &["install", "--yes"]);

  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let skill = temp.path().join(".claude/skills/compostbin-manifest");
  assert!(skill.join("SKILL.md").is_file());
  assert!(skill.join("references/host.md").is_file());
  assert!(temp.path().join(".config/compostbin/profiles").is_dir());

  let again = compostbin(temp.path(), temp.path(), &["install"]);
  assert!(again.status.success());
  assert!(String::from_utf8_lossy(&again.stdout).contains("up to date"));
}
