//! Turning compostbin's specs into what `containerization_framework` takes.
//!
//! Everything compostbin decides and the framework does not: which host
//! variables an `Inherit` resolves against, how a session or build is laid
//! onto the VM, and what a builder container is called.

use super::cache::Keys;
use super::nat;
use crate::model::{BuildPlan, EnvVar, Resources, RunSpec, SocketRelay};
use containerization_framework::{self as framework, model};
use std::path::Path;

/// `NAME=VALUE`.
///
/// An `Inherit` unset on the host is dropped, not passed empty, so the guest
/// can tell unset from empty. This is the only place the host environment is
/// read; the framework crate takes values already resolved.
pub fn environment(env: &[EnvVar]) -> Vec<String> {
  env
    .iter()
    .filter_map(|variable| match variable {
      EnvVar::Inherit(name) => std::env::var(name)
        .ok()
        .map(|value| format!("{name}={value}")),
      EnvVar::Set { name, value } => Some(format!("{name}={value}")),
    })
    .collect()
}

/// The guest's working directory; `/` when none is declared.
pub fn working_directory(workdir: Option<&Path>) -> String {
  workdir.map_or_else(|| "/".to_string(), |path| path.display().to_string())
}

/// A virtiofs share, read-only if declared so.
fn share(source: &Path, destination: impl Into<String>, readonly: bool) -> model::Mount {
  let options: &[&str] = if readonly { &["ro"] } else { &[] };

  model::Mount::share(source.display().to_string(), destination, options)
}

/// Mode `0o666`, so an unprivileged guest user can connect.
fn socket(socket: &SocketRelay) -> model::UnixSocketConfiguration {
  model::UnixSocketConfiguration {
    permissions: Some(0o666),
    ..model::UnixSocketConfiguration::new(socket.source.clone(), socket.target.clone())
  }
}

/// The container's limits plus a core and the guest kernel's memory, so the
/// container gets all it was given.
fn vm(resources: Resources) -> model::VmResources {
  model::VmResources {
    cpus: resources.cpus + 1,
    memory_in_bytes: resources.memory_in_bytes + model::VmResources::GUEST_MEMORY_OVERHEAD,
  }
}

/// A session's container, on its own address.
pub fn boot(spec: &RunSpec) -> framework::BootSpec {
  let mut boot = framework::BootSpec::new(spec.name.clone(), spec.image.clone());
  boot.vm = vm(spec.resources);

  let configuration = &mut boot.configuration;
  configuration.cpus = spec.resources.cpus;
  configuration.memory_in_bytes = spec.resources.memory_in_bytes;
  configuration.process = model::LinuxProcessConfiguration {
    arguments: Some(spec.arguments.clone()),
    environment_variables: environment(&spec.env),
    working_directory: Some(working_directory(spec.workdir.as_deref())),
    user: None,
  };
  configuration.interfaces = vec![nat::interface(&spec.name)];
  configuration.dns = Some(nat::dns());
  configuration.mounts.extend(
    spec
      .mounts
      .iter()
      .map(|mount| share(&mount.source, mount.target.display().to_string(), mount.readonly)),
  );
  configuration.sockets = spec.sockets.iter().map(socket).collect();

  boot
}

/// A build, with the caller's keys and a builder container of its own.
///
/// The image every compostbin build produces is a Debian derivative, so the
/// default `bash -euo pipefail -c` holds and nothing overrides the shell.
pub fn build(plan: &BuildPlan, keys: Keys, name: String) -> framework::BuildPlan {
  let interface = nat::interface(&name);

  framework::BuildPlan {
    cpus: plan.resources.cpus,
    memory_in_bytes: plan.resources.memory_in_bytes,
    vm: vm(plan.resources),
    mounts: plan
      .mounts
      .iter()
      .map(|mount| share(&mount.source, mount.destination.clone(), true))
      .collect(),
    steps: plan
      .steps
      .iter()
      .zip(keys.steps)
      .map(|(step, cache_key)| framework::BuildStep {
        name: step.name.clone(),
        script: step.script.clone(),
        user: step.user.clone(),
        cache_key,
      })
      .collect(),
    environment: plan.environment.clone(),
    labels: plan.labels.clone(),
    user: plan.user.clone(),
    workdir: plan.workdir.clone(),
    cache: framework::CachePolicy {
      restore: plan.cache,
      ..framework::CachePolicy::default()
    },
    ..framework::BuildPlan::new(name, plan.base.clone(), plan.tag.clone(), interface, keys.base)
  }
}

/// The builder container's id (and store directory). Random, so concurrent
/// builds never share a rootfs; short, as Containerization caps ids at 64.
pub fn builder_name() -> Result<String, getrandom::Error> {
  let mut bytes = [0u8; 4];
  getrandom::fill(&mut bytes)?;

  Ok(format!("cb-builder-{:08x}", u32::from_be_bytes(bytes)))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::Mount;
  use std::path::PathBuf;

  #[test]
  fn sets_a_variable_and_drops_an_unset_inherited_one() {
    // SAFETY: single-threaded test, and the name is this test's own.
    unsafe { std::env::set_var("BRIDGE_SPEC_TEST", "present") };

    let declared = [
      EnvVar::Inherit("BRIDGE_SPEC_TEST".to_string()),
      EnvVar::Inherit("BRIDGE_SPEC_TEST_UNSET".to_string()),
      EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      },
    ];

    assert_eq!(environment(&declared), ["BRIDGE_SPEC_TEST=present", "IS_SANDBOX=1"]);
  }

  #[test]
  fn names_a_builder() {
    let name = builder_name().expect("the OS should supply randomness");
    let digits = name
      .strip_prefix("cb-builder-")
      .expect("a cb-builder prefix");

    assert_eq!(digits.len(), 8);
    assert!(digits.chars().all(|digit| digit.is_ascii_hexdigit()));
  }

  fn run_spec() -> RunSpec {
    RunSpec {
      arguments: vec!["/bin/sleep".to_string(), "infinity".to_string()],
      env: vec![EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      }],
      image: "compostbin/base:latest".to_string(),
      mounts: vec![Mount {
        readonly: true,
        source: PathBuf::from("/Users/user/workspace"),
        target: PathBuf::from("/workspace"),
      }],
      name: "session-one".to_string(),
      resources: Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
      sockets: vec![SocketRelay {
        source: PathBuf::from("/state/ports/7001.sock"),
        target: PathBuf::from("/run/session/ports/7001.sock"),
      }],
      workdir: Some(PathBuf::from("/workspace")),
    }
  }

  #[test]
  fn boots_a_session_onto_its_own_nat_address() {
    let spec = boot(&run_spec());

    assert_eq!(spec.id, "session-one");
    assert_eq!(spec.configuration.interfaces, [nat::interface("session-one")]);
    assert_eq!(spec.configuration.dns, Some(nat::dns()));
    assert_eq!(spec.configuration.process.environment_variables, ["IS_SANDBOX=1"]);
  }

  #[test]
  fn adds_declared_mounts_after_the_standard_ones() {
    let spec = boot(&run_spec());
    let standard = model::LinuxContainerConfiguration::default_mounts();
    let (defaults, declared) = spec.configuration.mounts.split_at(standard.len());

    assert_eq!(defaults, standard);
    assert_eq!(
      declared,
      [model::Mount::share("/Users/user/workspace", "/workspace", &["ro"])]
    );
  }

  #[test]
  fn sizes_the_vm_to_hold_the_whole_container() {
    let spec = boot(&run_spec());

    assert_eq!(spec.configuration.cpus, 4);
    assert_eq!(spec.vm.cpus, 5);
    assert_eq!(
      spec.vm.memory_in_bytes,
      (8 << 30) + model::VmResources::GUEST_MEMORY_OVERHEAD
    );
  }

  /// Only the guest reaches host services, never the reverse.
  #[test]
  fn relays_every_socket_into_the_guest() {
    let spec = boot(&run_spec());

    assert_eq!(spec.configuration.sockets[0].direction, model::Direction::Into);
    assert_eq!(spec.configuration.sockets[0].permissions, Some(0o666));
  }

  #[test]
  fn gives_a_build_the_keys_compostbin_derived() {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );
    plan.steps = vec![crate::model::BuildStep::root("packages", "apt-get update")];

    let keys = crate::containerization::cache::keys(&plan).expect("a plan with no mount should key");
    let built = build(&plan, keys.clone(), "cb-builder-0123abcd".to_string());

    assert_eq!(built.name, "cb-builder-0123abcd");
    assert_eq!(built.base_key, keys.base);
    assert_eq!(built.steps[0].cache_key, keys.steps[0]);
    assert!(built.cache.restore);
  }

  #[test]
  fn says_when_a_build_is_not_to_read_its_cache() {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );
    plan.cache = false;

    let keys = crate::containerization::cache::keys(&plan).expect("a plan should key");

    assert!(
      !build(&plan, keys, "cb-builder-0123abcd".to_string())
        .cache
        .restore
    );
  }
}
