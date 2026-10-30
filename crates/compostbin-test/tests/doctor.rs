#![cfg(feature = "integration")]

use compostbin_test::{Project, code, poll_until, stderr, stdout, unused_port};
use std::time::Duration;

const START_TIMEOUT: Duration = Duration::from_secs(60);

const CHECKS: [&str; 11] = [
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
  "host sockets",
];

#[test]
fn healthy_project_passes() {
  let project = Project::new("cbt-doctor");

  let output = project.doctor();
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

#[test]
fn built_base_image_is_reported() {
  let project = Project::new("cbt-doctor-image");

  let report = stdout(&project.doctor());
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
  project.mount(&absent, false);

  let output = project.doctor();
  let report = stdout(&output);

  assert_eq!(code(&output), 1, "a failing check makes doctor exit non-zero");
  assert!(
    report.contains("FAIL  mounted paths") && report.contains(&absent.display().to_string()),
    "the missing path should be named:\n{report}"
  );
}

#[test]
fn running_container_is_seen() {
  let project = Project::new("cbt-doctor-live");

  let before = stdout(&project.doctor());
  assert!(
    before.contains("is not running"),
    "nothing is up before the first command:\n{before}"
  );

  let running = project.guest_in_background("sleep 60");
  let during = once_running(&project);

  assert!(
    during.contains("ok    container mounts") && during.contains("the mounts the manifest declares"),
    "a live container should be seen with what it was started with:\n{during}"
  );

  drop(running);
}

#[test]
fn mid_session_edit_is_drift() {
  let project = Project::new("cbt-doctor-drift");
  let libfoo = project.sibling("libfoo", &[]);

  let running = project.guest_in_background("sleep 60");
  once_running(&project);

  project.mount(&libfoo, false);

  let report = stdout(&project.doctor());

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

#[test]
fn idle_port_is_reported() {
  let project = Project::new("cbt-doctor-ports");
  let port = unused_port();
  project.manifest(&format!(
    r#"
[host]
ports = [{port}]
"#
  ));

  let output = project.doctor();
  let report = stdout(&output);

  assert_eq!(code(&output), 0, "a service that has not started yet is not a failure");
  assert!(
    report.contains(&format!("localhost:{port} — nothing is listening")),
    "the report should name the port:\n{report}"
  );
}

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

  let report = stdout(&project.doctor());

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

fn once_running(project: &Project) -> String {
  poll_until(START_TIMEOUT, "doctor to see the session running", || {
    Some(stdout(&project.doctor())).filter(|report| !report.contains("is not running"))
  })
}
