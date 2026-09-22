#![cfg(feature = "integration")]
//! `[container]`: the size of the machine, what it inherits, and what runs
//! before anything else does.

use compostbin_test::{Project, stderr, stdout};

/// What the manifest declares is a floor, not an exact count: the guest
/// reports one processor and a little memory more than was asked for, which
/// belong to the VM rather than to the container. So this asserts what
/// compostbin does promise — that a session is at least the machine it asked
/// for, and that asking for more gets more.
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

  // Neither exactly nor at least: a guest reports a little under what it was
  // given once the kernel has taken its share, and a small VM a little over.
  // Near enough is the whole claim — a setting that was ignored would be out
  // by the difference between these two.
  for (declared, reported) in [(1u64 << 20, small_memory), (3 << 20, large_memory)] {
    let apart = reported.abs_diff(declared);

    assert!(
      apart < declared / 5,
      "a machine declared {declared} KiB reported {reported}, which is not the size it asked for"
    );
  }
}

/// The processors and the kibibytes of memory the guest reports, in one
/// container: each call here is a container of its own.
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

/// Named in the manifest, set on this side: the value comes from the host
/// environment, so a session gets the developer's token without the manifest
/// holding it.
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

/// Setup runs where the project is mounted, with the workspace available —
/// which is what distinguishes it from an `[image]` step.
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

/// A failing line stops Claude, since a session without its setup is not the
/// session that was asked for. It does not stop an `exec`, which may well be
/// what is debugging the failure.
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
