use crate::output::{stderr, stdout};
use crate::project::Project;
use compostbin_core::image::{self, IMAGE_STORE};
use compostbin_core::session::Session;
use compostbin_engine::containerization::{FrameworkBuilder, FrameworkEngine, Store};
use compostbin_engine::engine::Engine;
use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SUITE_CACHE_VAR: &str = "COMPOSTBIN_TEST_CACHE";
pub const REMOVE_AFTER: &str = "--remove-after";
const RUN_POLL: Duration = Duration::from_secs(1);

/// Runs once as nextest's setup script; nextest has no teardown, so a detached
/// process removes the suite directory after nextest exits.
pub fn prepare() {
  let suite = tempfile::Builder::new()
    .prefix("compostbin-test-")
    .tempdir()
    .expect("create the suite's directory")
    .keep();
  remove_once_exited(nextest_pid(), &suite);

  let cache = suite.join("cache");
  clone(&real_cache(), &cache);
  export(SUITE_CACHE_VAR, &cache);

  let project = Project::sharing_cache("cbt-prepare", &cache);
  assert_bootable(&project.session());

  let output = project.guest("true");
  assert!(
    output.status.success(),
    "the base image should unpack and boot: {}",
    stderr(&output)
  );
}

pub fn remove_after(pid: libc::pid_t, directory: &Path) {
  // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
  while unsafe { libc::kill(pid, 0) } == 0 {
    std::thread::sleep(RUN_POLL);
  }

  let _ = std::fs::remove_dir_all(directory);
}

pub(crate) fn suite_cache() -> PathBuf {
  let cache = std::env::var_os(SUITE_CACHE_VAR).map(PathBuf::from);

  cache.filter(|cache| cache.is_dir()).unwrap_or_else(|| {
    panic!(
      "no suite cache in ${SUITE_CACHE_VAR}; run the suite with `cargo nextest run`, whose setup script prepares it"
    )
  })
}

pub(crate) fn clone(source: &Path, destination: &Path) {
  assert!(
    source.is_dir(),
    "no image store at {}; run `compostbin build` first",
    source.display()
  );

  let c_path = |path: &Path| CString::new(path.as_os_str().as_bytes()).expect("a path without a NUL");
  let (source_c, destination_c) = (c_path(source), c_path(destination));

  // SAFETY: two NUL-terminated paths that outlive the call.
  let status = unsafe { libc::clonefile(source_c.as_ptr(), destination_c.as_ptr(), 0) };

  assert_eq!(
    status,
    0,
    "clone {} to {}: {}",
    source.display(),
    destination.display(),
    std::io::Error::last_os_error()
  );
}

/// Its own process group so Ctrl-C doesn't stop it, and no inherited streams so
/// nextest isn't left waiting on them.
fn remove_once_exited(pid: libc::pid_t, directory: &Path) {
  Command::new(std::env::current_exe().expect("prepare-store has a path"))
    .arg(REMOVE_AFTER)
    .arg(pid.to_string())
    .arg(directory)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0)
    .spawn()
    .expect("start the process that cleans up after the run");
}

/// Searches ancestors, not just the parent, since the script may run under
/// `cargo run`.
fn nextest_pid() -> libc::pid_t {
  // SAFETY: always succeeds.
  let mut pid = unsafe { libc::getppid() };

  while pid > 1 {
    if ps(pid, "comm").ends_with("cargo-nextest") {
      return pid;
    }
    pid = ps(pid, "ppid").parse().expect("ps should print a pid");
  }

  panic!("prepare-store should run as nextest's setup script");
}

fn ps(pid: libc::pid_t, field: &str) -> String {
  let output = Command::new("ps")
    .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
    .output()
    .expect("run ps");

  stdout(&output).trim().to_string()
}

fn export(name: &str, value: &Path) {
  let path = std::env::var_os("NEXTEST_ENV").expect("prepare-store should run as nextest's setup script");
  let mut file = std::fs::OpenOptions::new()
    .append(true)
    .open(path)
    .expect("open nextest's environment file");

  writeln!(file, "{name}={}", value.display()).expect("write nextest's environment file");
}

fn assert_bootable(session: &Session) {
  let store = Store::at(image::store(session.resolver()));

  if let Err(error) = store.ready() {
    panic!("{error}; run `compostbin build`");
  }

  let wanted = &session.manifest.project.image;

  match FrameworkEngine::new(session.sessions_dir(), store.clone()).missing_content(wanted) {
    Ok(None) => {}
    Ok(Some(digest)) => panic!("{wanted} is missing blob {digest}; run `compostbin build`"),
    Err(error) => panic!("{error}; run `compostbin build`"),
  }

  let builder = FrameworkBuilder::new(store);
  let plans = image::plans(session).unwrap_or_else(|error| panic!("{error}"));

  for plan in plans {
    match builder.is_built(&plan) {
      Ok(true) => {}
      Ok(false) => panic!("{} is out of date with this checkout; run `compostbin build`", plan.tag),
      Err(error) => panic!("{error}; run `compostbin build`"),
    }
  }
}

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
