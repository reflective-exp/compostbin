//! A throwaway project, and the session it starts.

use crate::output::{stderr, stdout};
use crate::signing::signed_binary;
use crate::terminal::Terminal;
use compostbin_core::manifest::{MANIFEST_RELATIVE_PATH, Manifest, Memory, TomlFile};
use compostbin_core::session::NAME_PREFIX;
use compostbin_core::session::image::IMAGE_STORE;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Where a project's temporary home goes. Not the system temp directory, whose
/// path on macOS is long enough that a port socket under it — `<home>/.local/
/// state/compostbin/sessions/<name>/ports/<port>.sock` — exceeds the 104 bytes
/// a unix socket path may have.
const HOME_PARENT: &str = "/tmp";
/// How long [`Project::wait_for`] gives the guest. Generous, because the first
/// command in a project is waiting on a container to be created.
const GUEST_TIMEOUT: Duration = Duration::from_secs(60);
const GUEST_POLL: Duration = Duration::from_millis(50);
/// What a session gets unless the test asks for something else: enough to run
/// a shell, little enough that the whole suite can run at once.
const TEST_CPUS: u32 = 1;
const TEST_MEMORY: Memory = Memory::gibibytes(1);
/// `Notice::Shared`, the one thing compostbin prints on the stdout an attached
/// process is about to write to.
const SHARED_NOTICE: &str = "shared from your own ~/.claude:";

/// A project with a manifest, a home of its own, and a container named after
/// it.
///
/// The name must be unique across the suite: it names the container, and two
/// tests running at once must not be the same session. Dropping it takes the
/// home — and so the session's state — with it.
pub struct Project {
  /// Held only to be dropped: that is what removes the home, and with it the
  /// session's state. `home()` is the canonical path to the same directory.
  _home: tempfile::TempDir,
  name: String,
  dir: PathBuf,
}

impl Project {
  /// A project directory under a fresh home, holding the default manifest.
  /// Call [`Project::manifest`] before the first command to change it: the
  /// container is created by the first one that needs it.
  pub fn new(name: &str) -> Self {
    let home = tempfile::TempDir::new_in(HOME_PARENT).expect("temp home");
    // Canonical, so an assertion comparing host paths is not defeated by
    // macOS's `/tmp` -> `/private/tmp` symlink.
    let root = home.path().canonicalize().expect("canonical home");
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create project dir");

    // The store is the one thing not thrown away: building the base image again
    // per test would cost minutes each. Everything else under this home is the
    // test's own.
    let cache = real_cache();
    std::fs::create_dir_all(&cache).expect("create the shared cache");
    std::fs::create_dir_all(root.join(".cache")).expect("create cache dir");
    std::os::unix::fs::symlink(&cache, root.join(".cache/compostbin")).expect("link the image store");

    let project = Self {
      _home: home,
      dir,
      name: name.to_string(),
    };
    project.manifest("");

    project
  }

  /// Writes `body` as the manifest, with what every test needs filled in: the
  /// project name, and a session that never reads the login Keychain — an
  /// ad-hoc signed binary has a new code identity each build, so a test that
  /// asked would block on a keychain prompt nobody is there to answer.
  ///
  /// A body that says nothing about the size of the machine gets a small one:
  /// the suite runs these sessions in parallel, and a default 8 GiB each would
  /// have the host swapping rather than testing.
  ///
  /// Written through `Manifest` rather than as text, so a body that a session
  /// could not load fails here, in the test that wrote it.
  pub fn manifest(&self, body: &str) {
    let declared: toml::Table = toml::from_str(body).expect("the manifest body should parse");
    let mut manifest: Manifest = toml::from_str(body).expect("the manifest body should parse");

    manifest.claude.seed_from_keychain = false;
    manifest.project.name = Some(self.name.clone());

    if !declares(&declared, "cpus") {
      manifest.container.cpus = TEST_CPUS;
    }
    if !declares(&declared, "memory") {
      manifest.container.memory = TEST_MEMORY;
    }

    manifest
      .save(&self.manifest_path())
      .expect("write the manifest");
  }

  pub fn dir(&self) -> &Path {
    &self.dir
  }

  /// The home this project's session keeps its state under, and the `~` its
  /// manifest resolves against.
  pub fn home(&self) -> &Path {
    self
      .dir
      .parent()
      .expect("the project directory is under the home")
  }

  pub fn manifest_path(&self) -> PathBuf {
    self.dir.join(MANIFEST_RELATIVE_PATH)
  }

  /// What the container is called, and so what the session's state directory
  /// is named after.
  pub fn container_name(&self) -> String {
    format!("{NAME_PREFIX}{}", self.name)
  }

  pub fn state_dir(&self) -> PathBuf {
    self
      .home()
      .join(".local/state/compostbin/sessions")
      .join(self.container_name())
  }

  /// Writes a file in the project directory, creating its parents.
  pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
    let path = self.dir.join(relative);
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent).expect("create parent directory");
    }
    std::fs::write(&path, contents).expect("write file");

    path
  }

  /// Reads a file from the project directory, for the guest-wrote-it direction.
  pub fn read(&self, relative: &str) -> String {
    std::fs::read_to_string(self.dir.join(relative)).expect("read file")
  }

  /// Waits for a file in the project directory to have something in it, which
  /// is how a test watching a running session learns where the guest has got
  /// to.
  ///
  /// Its contents, not its existence: the guest's `>` creates the file before
  /// the command it redirects has run, so waiting to see it would let the test
  /// act on a command still in flight.
  pub fn wait_for(&self, relative: &str) {
    let path = self.dir.join(relative);
    let deadline = std::time::Instant::now() + GUEST_TIMEOUT;

    while !std::fs::read(&path).is_ok_and(|body| !body.is_empty()) {
      assert!(
        std::time::Instant::now() < deadline,
        "the guest never wrote {}",
        path.display()
      );
      std::thread::sleep(GUEST_POLL);
    }
  }

  /// Runs `compostbin` in the project directory, on this project's home.
  pub fn compostbin(&self, arguments: &[&str]) -> Output {
    self.invoke(arguments, None, &[])
  }

  /// The same, with `input` on the command's stdin.
  pub fn compostbin_with_input(&self, arguments: &[&str], input: &str) -> Output {
    self.invoke(arguments, Some(input), &[])
  }

  /// The same, with extra environment variables — what `[container] env` names
  /// has to be set on this side to be passed through.
  pub fn compostbin_with_env(&self, arguments: &[&str], env: &[(&str, &str)]) -> Output {
    self.invoke(arguments, None, env)
  }

  /// Runs a shell line in the guest, which is what most of these tests assert
  /// on. Creates the container if it is not already up.
  pub fn guest(&self, script: &str) -> Output {
    self.compostbin(&["exec", "sh", "-c", one_line(script)])
  }

  /// The same, with `input` reaching the guest's stdin.
  pub fn guest_with_input(&self, script: &str, input: &str) -> Output {
    self.compostbin_with_input(&["exec", "sh", "-c", one_line(script)], input)
  }

  /// A guest line that must succeed, as its trimmed stdout — the guest's own,
  /// without the notice compostbin prints before attaching it.
  pub fn guest_output(&self, script: &str) -> String {
    let output = self.guest(script);
    assert!(
      output.status.success(),
      "`{script}` failed in the guest: {}{}",
      stdout(&output),
      stderr(&output)
    );

    stdout(&output)
      .lines()
      .filter(|line| !line.starts_with(SHARED_NOTICE))
      .collect::<Vec<_>>()
      .join("\n")
      .trim()
      .to_string()
  }

  /// Starts a guest process and leaves it running, for a test that has to
  /// watch the session from this side while something happens in it. The guard
  /// kills it if the test ends first.
  /// Its stdin stays open: a test tells such a process to go on with
  /// [`Running::send`], which is the one channel into a running guest that is
  /// immediate. A host file written into the mount is not — a guest that has
  /// already looked for it can go on not seeing it indefinitely.
  pub fn guest_in_background(&self, script: &str) -> Running {
    Running(Some(
      self
        .command(&["exec", "sh", "-c", one_line(script)], &[])
        .spawn()
        .expect("compostbin should start"),
    ))
  }

  /// Runs `compostbin` on a terminal of its own, which is the only way to reach
  /// what it does when the caller has one: `exec -t`, `shell`, and a
  /// `tty = true` host command.
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
    // Closed either way: a guest process reading to EOF would otherwise wait
    // for a stdin nobody is going to write to.
    drop(stdin);

    child.wait_with_output().expect("compostbin should finish")
  }

  fn command(&self, arguments: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(signed_binary());
    command
      .args(arguments)
      .current_dir(&self.dir)
      .env("HOME", self.home())
      // Whether the host has one decides a doctor check and nothing else, so
      // it is never inherited: a developer with a key set would see different
      // results from CI.
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

/// A guest process still running. Killing the `compostbin` that created the
/// container ends the container too, so a test that leaves one behind would
/// leave a VM behind.
pub struct Running(Option<std::process::Child>);

impl Running {
  /// Writes a line to the guest process's stdin, for a script stepping through
  /// a test with `read`. The only prompt way to reach a running guest: a file
  /// written into the mount from here may not be seen there for minutes.
  pub fn send(&mut self, line: &str) {
    let child = self.0.as_mut().expect("the process is still running");
    let stdin = child.stdin.as_mut().expect("piped stdin");

    writeln!(stdin, "{line}").expect("write to the guest's stdin");
    stdin.flush().expect("flush the guest's stdin");
  }

  /// Waits for the process and reports what it wrote. The container it created
  /// goes when it does.
  pub fn finish(mut self) -> Output {
    let mut child = self.0.take().expect("a running process is taken once");

    // Closed first: a script that reads to the end of its input would
    // otherwise wait for a stdin this test is done with.
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

impl Drop for Project {
  /// The home goes with the `TempDir`, but the container's unpacked rootfs
  /// lives in the shared store, under this project's name. Left behind it
  /// would be one stale clone per test, forever.
  fn drop(&mut self) {
    let containers = real_cache().join("images/containers");
    let _ = std::fs::remove_dir_all(containers.join(self.container_name()));
  }
}

/// A script, checked for the newline that would quietly cut it short.
///
/// Arguments cross to the guest as newline-separated lines (see the
/// `containerization-framework` wire format), so a two-line script arrives as
/// two arguments and `sh -c` runs only the first. A test that hits this would
/// otherwise see its later lines silently not happen; separate them with `;`.
fn one_line(script: &str) -> &str {
  assert!(
    !script.contains('\n'),
    "a guest script crosses as one argument, so it cannot contain a newline: {script:?}"
  );

  script
}

/// Whether the body being written said anything about `[container] <key>`, so
/// a test that chose a machine size keeps it.
fn declares(body: &toml::Table, key: &str) -> bool {
  body
    .get("container")
    .and_then(toml::Value::as_table)
    .is_some_and(|container| container.contains_key(key))
}

/// The cache holding the image store the developer already built, which every
/// project borrows: the images, the kernel, and the build context beside them.
fn real_cache() -> PathBuf {
  let home = std::env::var("HOME").expect("HOME");
  let store = IMAGE_STORE
    .strip_prefix("~/")
    .expect("the store is under the home");

  Path::new(&home).join(
    Path::new(store)
      .parent()
      .expect("the store is inside the cache"),
  )
}
