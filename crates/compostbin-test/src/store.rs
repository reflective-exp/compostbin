use crate::output::{stderr, stdout};
use crate::project::Project;
use crate::signing::target_dir;
use compostbin_core::image::IMAGE_STORE;
use std::ffi::CString;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const SUITE_CACHE_VAR: &str = "COMPOSTBIN_TEST_CACHE";
const SUITES: &str = "compostbin-test";

/// Runs once as nextest's setup script. Nextest has no teardown, so each run's
/// directory is named for its nextest pid and removed by a later run once that
/// pid has exited.
pub fn prepare() {
  let suites = target_dir().join(SUITES);
  std::fs::create_dir_all(&suites).expect("create the suites' directory");
  remove_finished(&suites);

  let suite = suites.join(nextest_pid().to_string());
  std::fs::create_dir(&suite).expect("create the suite's directory");

  let cache = suite.join("cache");
  seed(&cache);
  export(SUITE_CACHE_VAR, &cache);

  let project = Project::sharing_cache("cbt-prepare", &cache);

  let output = project.compostbin(&["build"]);
  assert!(
    output.status.success(),
    "the base image should build: {}",
    stderr(&output)
  );

  let output = project.guest("true");
  assert!(
    output.status.success(),
    "the base image should unpack and boot: {}",
    stderr(&output)
  );
}

pub(crate) fn suite_cache() -> PathBuf {
  let cache = std::env::var_os(SUITE_CACHE_VAR).map(PathBuf::from);

  cache.filter(|cache| cache.is_dir()).unwrap_or_else(|| {
    panic!(
      "no suite cache in ${SUITE_CACHE_VAR}; run the suite with `cargo nextest run`, whose setup script prepares it"
    )
  })
}

/// Starts from the developer's cache when there is one, so the build only runs
/// the steps this checkout changed.
fn seed(cache: &Path) {
  let real = real_cache();

  if real.is_dir() {
    clone(&real, cache);
  } else {
    std::fs::create_dir(cache).expect("create the suite's cache");
  }
}

pub(crate) fn clone(source: &Path, destination: &Path) {
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

fn remove_finished(suites: &Path) {
  let entries = std::fs::read_dir(suites).expect("list the suites' directory");

  for entry in entries.flatten() {
    let running = entry
      .file_name()
      .to_str()
      .and_then(|name| name.parse::<libc::pid_t>().ok())
      // SAFETY: signal 0 delivers nothing; it only asks whether `pid` exists.
      .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);

    if !running {
      let _ = std::fs::remove_dir_all(entry.path());
    }
  }
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
