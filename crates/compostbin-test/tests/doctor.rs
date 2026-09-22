#![cfg(feature = "integration")]
//! `doctor`, against a session rather than a fixture.
//!
//! Every check is decided by a function with unit tests behind it. What those
//! cannot answer is whether the checks are asking the machine the right
//! questions: whether the store it reads is the one a session boots from, and
//! whether a container that is really running is seen as running.

use compostbin_test::{Project, code, stderr, stdout};

/// `doctor` fails a session that cannot authenticate, which one made by the
/// harness cannot: it never reads the Keychain. A key in the environment is
/// the other way to authenticate, and the one a test can arrange.
fn diagnose(project: &Project) -> std::process::Output {
  project.compostbin_with_env(&["doctor"], &[("ANTHROPIC_API_KEY", "not-a-real-key")])
}

/// The check names, so a check dropped from `doctor` is noticed here too.
const CHECKS: [&str; 10] = [
  "engine",
  "image store",
  "base image",
  "mounted paths",
  "dangling symlinks",
  "container mounts",
  "mount breadth",
  "credentials",
  "host commands",
  "host ports",
];

#[test]
fn healthy_project_passes() {
  let project = Project::new("cbt-doctor");

  let output = diagnose(&project);
  let report = stdout(&output);

  assert_eq!(code(&output), 0, "doctor failed: {report}{}", stderr(&output));
  for name in CHECKS {
    assert!(
      report.contains(&format!("{name}:")),
      "`{name}` is missing from the report:\n{report}"
    );
  }
  assert!(
    !report.contains("FAIL"),
    "nothing should be failing on a project that works:\n{report}"
  );
}

/// The store a session boots from is the one `doctor` reads, so a built base
/// image is reported as built — which is what tells a first-time user whether
/// `compostbin build` still has to run.
#[test]
fn built_base_image_is_reported() {
  let project = Project::new("cbt-doctor-image");

  let report = stdout(&diagnose(&project));
  let line = report
    .lines()
    .find(|line| line.contains("base image:"))
    .expect("a base image check");

  assert!(
    line.starts_with("ok"),
    "these tests need a built image, and this is where a missing one shows: {line}"
  );
}

#[test]
fn missing_path_fails() {
  let project = Project::new("cbt-doctor-missing");
  let absent = project.home().join("never-created");
  project.manifest(&format!(
    r#"
[[paths]]
source = "{}"
"#,
    absent.display()
  ));

  let output = diagnose(&project);
  let report = stdout(&output);

  assert_eq!(code(&output), 1, "a failing check makes doctor exit non-zero");
  assert!(
    report.contains("FAIL  mounted paths") && report.contains(&absent.display().to_string()),
    "the missing path should be named:\n{report}"
  );
}

/// A container is running only while the process that made it is, so this is
/// the one check that needs a live session.
#[test]
fn running_container_is_seen() {
  let project = Project::new("cbt-doctor-live");

  let before = stdout(&diagnose(&project));
  assert!(
    before.contains("is not running"),
    "nothing is up before the first command:\n{before}"
  );

  let running = project.guest_in_background("sleep 60");
  let during = wait_until(&project, |report| !report.contains("is not running"));

  assert!(
    during.contains("ok    container mounts") && during.contains("the mounts the manifest declares"),
    "a live container should be seen with what it was started with:\n{during}"
  );

  drop(running);
}

/// Mounts are fixed when the container is created, so a manifest edited during
/// a session is a gap `doctor` is there to name.
#[test]
fn mid_session_edit_is_drift() {
  let project = Project::new("cbt-doctor-drift");
  let libfoo = project.home().join("libfoo");
  std::fs::create_dir(&libfoo).expect("create libfoo");

  let running = project.guest_in_background("sleep 60");
  wait_until(&project, |report| !report.contains("is not running"));

  project.manifest(&format!(
    r#"
[[paths]]
source = "{}"
"#,
    libfoo.display()
  ));

  let report = stdout(&diagnose(&project));

  assert!(
    report.contains("warn  container mounts") && report.contains("exit it and `compostbin run` again"),
    "an edit the running container never got should say so, and how to fix it:\n{report}"
  );
  assert!(
    report.contains(&libfoo.display().to_string()),
    "and name what is missing from it:\n{report}"
  );

  drop(running);
}

/// A port whose service has not started is ordinary — `[host.commands]` may
/// start it — so it is a warning that names the likeliest reason a guest's
/// `localhost:<port>` refuses.
#[test]
fn idle_port_is_reported() {
  let project = Project::new("cbt-doctor-ports");
  // Bound and dropped, so the port is one nothing is listening on.
  let port = std::net::TcpListener::bind("127.0.0.1:0")
    .expect("bind")
    .local_addr()
    .expect("address")
    .port();
  project.manifest(&format!(
    r#"
[host]
ports = [{port}]
"#
  ));

  let output = diagnose(&project);
  let report = stdout(&output);

  assert_eq!(code(&output), 0, "a service that has not started yet is not a failure");
  assert!(
    report.contains(&format!("localhost:{port} — nothing is listening")),
    "the report should name the port:\n{report}"
  );
}

/// A command the guest can widen runs on the host with the user's own
/// privileges, so `doctor` lists every one and warns about the widened.
#[test]
fn host_commands_are_listed() {
  let project = Project::new("cbt-doctor-host");
  project.manifest(
    r#"
[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

[host.commands.test-one]
arguments = true
argv = ["cargo", "nextest", "run"]
"#,
  );

  let report = stdout(&diagnose(&project));

  assert!(
    report.contains("warn  host commands"),
    "a command that takes guest arguments is worth a warning:\n{report}"
  );
  assert!(
    report.contains("test = cargo nextest run --workspace")
      && report.contains("test-one = cargo nextest run (+ guest arguments)"),
    "every allowlisted command should be named:\n{report}"
  );
}

/// Polls `doctor` until the report says what the test is waiting for, since a
/// container takes a moment to come up.
fn wait_until(project: &Project, ready: impl Fn(&str) -> bool) -> String {
  let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

  loop {
    let report = stdout(&diagnose(project));

    if ready(&report) {
      return report;
    }

    assert!(
      std::time::Instant::now() < deadline,
      "the session never came up:\n{report}"
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
  }
}
