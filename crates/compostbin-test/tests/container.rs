#![cfg(feature = "integration")]

use compostbin_test::{Project, stderr, stdout};

/// The guest can report more processors than declared, so CPU counts are lower
/// bounds.
#[test]
fn machine_is_the_declared_size() {
  let small = Project::new("cbt-resources-small");
  small.manifest(
    r#"
[container]
cpus = 1
memory = "1G"
"#,
  );
  let large = Project::new("cbt-resources-large");
  large.manifest(
    r#"
[container]
cpus = 4
memory = "3G"
"#,
  );

  let (small_cpus, small_memory) = machine(&small);
  let (large_cpus, large_memory) = machine(&large);

  assert!(
    small_cpus >= 1 && large_cpus >= 4,
    "a session gets at least the processors it declared, not {small_cpus} and {large_cpus}"
  );
  assert!(
    small_cpus < large_cpus,
    "asking for more processors got the same {small_cpus}, so the setting does nothing"
  );

  // Approximate: reported memory lands a little under or over what was given.
  for (declared, reported) in [(1u64 << 20, small_memory), (3 << 20, large_memory)] {
    let apart = reported.abs_diff(declared);

    assert!(
      apart < declared / 5,
      "a machine declared {declared} KiB reported {reported}, which is not the size it asked for"
    );
  }
}

fn machine(project: &Project) -> (usize, u64) {
  let reported = project.guest_output("nproc; awk '/MemTotal/ { print $2 }' /proc/meminfo");
  let mut lines = reported.lines();

  (
    lines
      .next()
      .and_then(|count| count.trim().parse().ok())
      .expect("nproc prints a count"),
    lines
      .next()
      .and_then(|total| total.trim().parse().ok())
      .expect("MemTotal is a number of kibibytes"),
  )
}

#[test]
fn declared_env_passes_through() {
  let project = Project::new("cbt-env");
  project.manifest(
    r#"
[container]
env = ["GITHUB_TOKEN"]
"#,
  );

  let output = project.compostbin_with_env(
    &[
      "exec",
      "sh",
      "-c",
      "echo \"${GITHUB_TOKEN:-unset} ${OTHER_TOKEN:-unset}\"",
    ],
    &[("GITHUB_TOKEN", "from-the-host"), ("OTHER_TOKEN", "undeclared")],
  );

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    stdout(&output).trim(),
    "from-the-host unset",
    "only what the manifest names crosses into the container"
  );
}

#[test]
fn setup_runs_in_the_project() {
  let project = Project::new("cbt-setup");
  project.manifest(
    r#"
[container]
setup = ["pwd > setup-ran", "echo second >> setup-ran"]
"#,
  );

  assert_eq!(
    project.guest_output("cat setup-ran"),
    "/workspace/cbt-setup\nsecond",
    "each line runs, in order, in the project directory"
  );
  assert_eq!(
    project.read("setup-ran"),
    "/workspace/cbt-setup\nsecond\n",
    "and what it wrote is on the host, like any other guest write"
  );
}

#[test]
fn failing_setup_still_allows_exec() {
  let project = Project::new("cbt-setup-fails");
  project.manifest(
    r#"
[container]
setup = ["exit 3", "touch never-reached"]
"#,
  );

  let output = project.guest("echo attached anyway");

  assert!(output.status.success(), "an exec is not stopped by a failed setup");
  assert_eq!(stdout(&output), "attached anyway\n");
  assert!(
    stderr(&output).contains("setup `exit 3` exited 3"),
    "the failure must be reported, not swallowed: {}",
    stderr(&output)
  );
  assert!(
    !project.dir().join("never-reached").exists(),
    "setup stops at the first failing line"
  );
}
