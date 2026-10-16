use crate::output::{stderr, stdout};
use crate::poll::poll_until;
use crate::signing::signed_binary;
use crate::store;
use crate::terminal::Terminal;
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest, Memory, TomlFile};
use compostbin_core::session::Session;
use compostbin_core::workspace::paths::PathResolver;
use compostbin_engine::containerization::control_socket;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Not the system temp directory: on macOS its path makes port socket paths
/// exceed the 104-byte unix socket path limit.
const HOME_PARENT: &str = "/tmp";
/// Long because the first command in a project creates the container.
const GUEST_TIMEOUT: Duration = Duration::from_secs(60);
const FAKE_API_KEY: &str = "not-a-real-key";
const TEST_CPUS: u32 = 1;
const TEST_MEMORY: Memory = Memory::gibibytes(1);

/// The name must be unique across the suite because it names the container.
pub struct Project {
  _home: tempfile::TempDir,
  name: String,
  dir: PathBuf,
}

impl Project {
  /// Call [`Project::manifest`] before the first command, which creates the
  /// container.
  pub fn new(name: &str) -> Self {
    Self::with_cache(name, |cache| store::clone(&store::suite_cache(), cache))
  }

  pub(crate) fn sharing_cache(name: &str, cache: &Path) -> Self {
    Self::with_cache(name, |link| {
      std::os::unix::fs::symlink(cache, link).expect("link the shared cache");
    })
  }

  fn with_cache(name: &str, place: impl FnOnce(&Path)) -> Self {
    let home = tempfile::TempDir::new_in(HOME_PARENT).expect("temp home");
    // Canonical because macOS's `/tmp` is a symlink to `/private/tmp`.
    let root = home.path().canonicalize().expect("canonical home");
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create project dir");
    std::fs::create_dir_all(root.join(".cache")).expect("create cache dir");
    place(&root.join(".cache/compostbin"));

    let project = Self {
      _home: home,
      dir,
      name: name.to_string(),
    };
    project.manifest("");

    project
  }

  /// Disables Keychain seeding, which would block an ad-hoc signed binary on a
  /// prompt, and defaults to a small machine so parallel sessions don't swap.
  pub fn manifest(&self, body: &str) {
    let declared: toml::Table = toml::from_str(body).expect("the manifest body should parse");
    let declares_cpus = declares(&declared, "cpus");
    let declares_memory = declares(&declared, "memory");
    let mut manifest: Manifest = declared
      .try_into()
      .expect("the manifest body should be a manifest");

    manifest.claude.seed_from_keychain = false;
    manifest.project.name = Some(self.name.clone());

    if !declares_cpus {
      manifest.container.cpus = TEST_CPUS;
    }
    if !declares_memory {
      manifest.container.memory = TEST_MEMORY;
    }

    manifest
      .save(&self.manifest_path())
      .expect("write the manifest");
  }

  pub fn dir(&self) -> &Path {
    &self.dir
  }

  pub fn home(&self) -> &Path {
    self
      .dir
      .parent()
      .expect("the project directory is under the home")
  }

  pub fn manifest_path(&self) -> PathBuf {
    self.dir.join(MANIFEST_RELATIVE_PATH)
  }

  pub fn session(&self) -> Session {
    Session::load(None, PathResolver::new(&self.dir, self.home()), &self.dir).expect("the manifest should load")
  }

  pub fn control_socket(&self) -> PathBuf {
    let session = self.session();

    control_socket(&session.sessions_dir(), &session.container_name())
  }

  pub fn sibling(&self, name: &str, files: &[(&str, &str)]) -> PathBuf {
    let path = self.home().join(name);
    std::fs::create_dir(&path).expect("create the sibling");
    for (file, contents) in files {
      std::fs::write(path.join(file), contents).expect("write into the sibling");
    }

    path
  }

  /// Replaces the manifest with one that mounts only `path`.
  pub fn mount(&self, path: &Path, readonly: bool) {
    self.manifest(&format!(
      r#"
[[paths]]
readonly = {readonly}
source = "{}"
"#,
      path.display()
    ));
  }

  pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
    let path = self.dir.join(relative);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).expect("create parent directory");
    }
    std::fs::write(&path, contents).expect("write file");

    path
  }

  pub fn read(&self, relative: &str) -> String {
    std::fs::read_to_string(self.dir.join(relative)).expect("read file")
  }

  /// Waits for contents rather than existence, because the guest's `>` creates
  /// the file before the command it redirects has run.
  pub fn wait_for(&self, relative: &str) {
    let path = self.dir.join(relative);

    poll_until(GUEST_TIMEOUT, &format!("the guest to write {}", path.display()), || {
      std::fs::read(&path)
        .is_ok_and(|body| !body.is_empty())
        .then_some(())
    });
  }

  pub fn compostbin(&self, arguments: &[&str]) -> Output {
    self.invoke(arguments, None, &[])
  }

  pub fn compostbin_with_env(&self, arguments: &[&str], env: &[(&str, &str)]) -> Output {
    self.invoke(arguments, None, env)
  }

  pub fn doctor(&self) -> Output {
    self.compostbin_with_env(&["doctor"], &[("ANTHROPIC_API_KEY", FAKE_API_KEY)])
  }

  pub fn guest(&self, script: &str) -> Output {
    self.compostbin(&["exec", "sh", "-c", one_line(script)])
  }

  pub fn guest_with_input(&self, script: &str, input: &str) -> Output {
    self.invoke(&["exec", "sh", "-c", one_line(script)], Some(input), &[])
  }

  pub fn guest_output(&self, script: &str) -> String {
    let output = self.guest(script);
    assert!(
      output.status.success(),
      "`{script}` failed in the guest: {}{}",
      stdout(&output),
      stderr(&output)
    );

    stdout(&output).trim().to_string()
  }

  pub fn guest_in_background(&self, script: &str) -> Running {
    Running(Some(
      self
        .command(&["exec", "sh", "-c", one_line(script)], &[])
        .spawn()
        .expect("compostbin should start"),
    ))
  }

  pub fn on_a_terminal(&self, arguments: &[&str]) -> Terminal {
    Terminal::attach(self.command(arguments, &[]))
  }

  fn invoke(&self, arguments: &[&str], input: Option<&str>, env: &[(&str, &str)]) -> Output {
    let mut child = self
      .command(arguments, env)
      .spawn()
      .expect("compostbin should start");
    let mut stdin = child.stdin.take().expect("piped stdin");
    if let Some(input) = input {
      stdin.write_all(input.as_bytes()).expect("write stdin");
    }
    // Closed so a guest process reading to EOF does not hang.
    drop(stdin);

    child.wait_with_output().expect("compostbin should finish")
  }

  fn command(&self, arguments: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(signed_binary());
    command
      .args(arguments)
      .current_dir(&self.dir)
      .env("HOME", self.home())
      // Not inherited, so a developer's key doesn't change doctor results.
      .env_remove("ANTHROPIC_API_KEY")
      .stdin(Stdio::piped())
      .stdout(Stdio::piped())
      .stderr(Stdio::piped());

    for (name, value) in env {
      command.env(name, value);
    }

    command
  }
}

pub struct Running(Option<std::process::Child>);

impl Running {
  /// The only prompt way to reach a running guest: a file written into the
  /// mount from the host may take minutes to appear there.
  pub fn send(&mut self, line: &str) {
    let child = self.0.as_mut().expect("the process is still running");
    let stdin = child.stdin.as_mut().expect("piped stdin");

    writeln!(stdin, "{line}").expect("write to the guest's stdin");
    stdin.flush().expect("flush the guest's stdin");
  }

  pub fn finish(mut self) -> Output {
    let mut child = self.0.take().expect("a running process is taken once");

    drop(child.stdin.take());

    child
      .wait_with_output()
      .expect("the guest process should finish")
  }
}

impl Drop for Running {
  fn drop(&mut self) {
    if let Some(mut child) = self.0.take() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

/// Arguments cross to the guest newline-separated, so `sh -c` would run only
/// the first line of a multi-line script.
fn one_line(script: &str) -> &str {
  assert!(
    !script.contains('\n'),
    "a guest script crosses as one argument, so it cannot contain a newline: {script:?}"
  );

  script
}

fn declares(body: &toml::Table, key: &str) -> bool {
  body
    .get("container")
    .and_then(toml::Value::as_table)
    .is_some_and(|container| container.contains_key(key))
}
