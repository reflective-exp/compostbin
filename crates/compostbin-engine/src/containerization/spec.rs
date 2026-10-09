//! Turning compostbin's specs into what `containerization_framework` takes.
//!
//! Everything compostbin decides and the framework does not: which host
//! variables an `Inherit` resolves against, how a session is laid onto the VM,
//! and what a builder container is called.

use super::nat;
use crate::model::{EnvVar, Resources, RunSpec};
use containerization_framework::containerization::container::{Mount, linux_container};
use containerization_framework::containerization::vm::VMResources;
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
pub fn share(source: &Path, destination: impl Into<String>, readonly: bool) -> Mount {
  let options: &[&str] = if readonly { &["ro"] } else { &[] };

  Mount::share(source.display().to_string(), destination, options, &[])
}

/// The container's limits plus a core and the guest kernel's memory, so the
/// container gets all it was given.
pub fn vm(resources: Resources) -> VMResources {
  VMResources {
    cpus: resources.cpus + 1,
    memory_in_bytes: resources.memory_in_bytes + VMResources::GUEST_MEMORY_OVERHEAD,
  }
}

/// A session's container, on its own address, over the configuration the
/// manager seeded from its image.
pub fn configure(
  spec: &RunSpec,
) -> impl FnOnce(&mut linux_container::Configuration) -> Result<(), containerization_framework::Error> + Send + 'static
{
  let name = spec.name.clone();
  let resources = spec.resources;
  let arguments = spec.arguments.clone();
  let environment = environment(&spec.env);
  let working_directory = working_directory(spec.workdir.as_deref());
  let mounts: Vec<Mount> = spec
    .mounts
    .iter()
    .map(|mount| share(&mount.source, mount.target.display().to_string(), mount.readonly))
    .collect();

  move |configuration: &mut linux_container::Configuration| {
    configuration.cpus = resources.cpus;
    configuration.memory_in_bytes = resources.memory_in_bytes;
    configuration.process.arguments = arguments;
    // Last, so a variable the session sets beats the image's.
    configuration
      .process
      .environment_variables
      .extend(environment);
    configuration.process.working_directory = working_directory;
    configuration.interfaces = vec![nat::interface(&name)?];
    configuration.dns = Some(nat::dns());
    configuration.mounts.extend(mounts);
    Ok(())
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
  use crate::model::Mount as SessionMount;
  use containerization_framework::containerization::container::LinuxContainer;
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
      mounts: vec![SessionMount {
        readonly: true,
        source: PathBuf::from("/Users/user/workspace"),
        target: PathBuf::from("/workspace"),
      }],
      name: "session-one".to_string(),
      resources: Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
      workdir: Some(PathBuf::from("/workspace")),
    }
  }

  /// What the manager seeds from an image declaring `PATH`, then the spec.
  fn configured() -> linux_container::Configuration {
    let mut configuration = linux_container::Configuration::default();
    configuration.process.environment_variables = vec!["PATH=/usr/bin".to_string()];

    configure(&run_spec())(&mut configuration).unwrap();

    configuration
  }

  #[test]
  fn boots_a_session_onto_its_own_nat_address() {
    let configuration = configured();

    assert_eq!(configuration.interfaces, [nat::interface("session-one").unwrap()]);
    assert_eq!(configuration.dns, Some(nat::dns()));
    assert_eq!(configuration.process.working_directory, "/workspace");
  }

  #[test]
  fn adds_the_sessions_variables_after_the_images() {
    assert_eq!(
      configured().process.environment_variables,
      ["PATH=/usr/bin", "IS_SANDBOX=1"]
    );
  }

  #[test]
  fn adds_declared_mounts_after_the_standard_ones() {
    let configuration = configured();
    let standard = LinuxContainer::default_mounts();
    let (defaults, declared) = configuration.mounts.split_at(standard.len());

    assert_eq!(defaults, standard);
    assert_eq!(
      declared,
      [Mount::share("/Users/user/workspace", "/workspace", &["ro"], &[])]
    );
  }

  #[test]
  fn sizes_the_vm_to_hold_the_whole_container() {
    let resources = run_spec().resources;

    assert_eq!(configured().cpus, 4);
    assert_eq!(vm(resources).cpus, 5);
    assert_eq!(
      vm(resources).memory_in_bytes,
      (8 << 30) + VMResources::GUEST_MEMORY_OVERHEAD
    );
  }
}
