//! Turning compostbin's specs into what `containerization_framework` takes.
//!
//! Everything compostbin decides and the framework does not: which host
//! variables an `Inherit` resolves against, where a guest sits on the NAT
//! network, what a builder container is called, and how a build's cache keys
//! are derived.

use super::nat;
use crate::cache::Keys;
use crate::model::{BuildPlan, EnvVar, Mount, RunSpec, SocketRelay};
use containerization_framework as framework;
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

fn mounts(mounts: &[Mount]) -> Vec<framework::Mount> {
  mounts
    .iter()
    .map(|mount| framework::Mount {
      readonly: mount.readonly,
      source: mount.source.clone(),
      target: mount.target.clone(),
    })
    .collect()
}

fn sockets(sockets: &[SocketRelay]) -> Vec<framework::SocketRelay> {
  sockets
    .iter()
    .map(|socket| framework::SocketRelay::into_guest(socket.source.clone(), socket.target.clone()))
    .collect()
}

fn resources(resources: crate::model::Resources) -> framework::Resources {
  framework::Resources {
    cpus: resources.cpus,
    memory_in_bytes: resources.memory_in_bytes,
  }
}

/// A session's container, on its own address.
pub fn boot(spec: &RunSpec) -> framework::BootSpec {
  framework::BootSpec {
    arguments: spec.arguments.clone(),
    environment: environment(&spec.env),
    mounts: mounts(&spec.mounts),
    sockets: sockets(&spec.sockets),
    workdir: spec.workdir.clone(),
    ..framework::BootSpec::new(
      spec.name.clone(),
      spec.image.clone(),
      resources(spec.resources),
      nat::network(&spec.name),
    )
  }
}

/// A build, with the caller's keys and a builder container of its own.
///
/// The image every compostbin build produces is a Debian derivative, so the
/// default `bash -euo pipefail -c` holds and nothing overrides the shell.
pub fn build(plan: &BuildPlan, keys: &Keys, name: String) -> framework::BuildPlan {
  framework::BuildPlan {
    mounts: plan
      .mounts
      .iter()
      .map(|mount| framework::BuildMount {
        destination: mount.destination.clone(),
        readonly: mount.readonly,
        source: mount.source.clone(),
      })
      .collect(),
    steps: plan
      .steps
      .iter()
      .zip(&keys.steps)
      .map(|(step, key)| framework::BuildStep {
        name: step.name.clone(),
        script: step.script.clone(),
        user: step.user.clone(),
        cache_key: key.clone(),
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
    ..framework::BuildPlan::new(
      name.clone(),
      plan.base.clone(),
      plan.tag.clone(),
      resources(plan.resources),
      nat::network(&name),
      keys.base.clone(),
    )
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
  use crate::model::Resources;
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

    assert_eq!(spec.name, "session-one");
    assert_eq!(spec.network, nat::network("session-one"));
    assert_eq!(spec.environment, ["IS_SANDBOX=1"]);
    assert_eq!(spec.mounts.len(), 1);
    assert!(spec.mounts[0].readonly);
  }

  /// Only the guest reaches host services, never the reverse.
  #[test]
  fn relays_every_socket_into_the_guest() {
    let spec = boot(&run_spec());

    assert_eq!(spec.sockets[0].direction, framework::Direction::IntoGuest);
    assert_eq!(spec.sockets[0].mode, 0o666);
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

    let keys = crate::cache::keys(&plan).expect("a plan with no mount should key");
    let built = build(&plan, &keys, "cb-builder-0123abcd".to_string());

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

    let keys = crate::cache::keys(&plan).expect("a plan should key");

    assert!(
      !build(&plan, &keys, "cb-builder-0123abcd".to_string())
        .cache
        .restore
    );
  }
}
