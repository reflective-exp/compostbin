//! Building images.
//!
//! Pull the base, unpack it to a writable ext4 block, boot it, run each step,
//! export the block to a tar, and store that as a single-layer image — all
//! in-process, with no daemon and no builder image.
//!
//! The rootfs is snapshotted after each step under that step's
//! [`cache_key`](crate::BuildStep::cache_key); a rebuild resumes from the
//! deepest match. The image is one layer regardless of step count: the cache is
//! block snapshots, not layers.

use crate::error::Error;
use crate::model::{BuildMount, BuildPlan, BuildStep};
use crate::store::{INITFS_REFERENCE, KERNEL_IN_ARCHIVE, KERNEL_URL, Store};
use crate::{checked, ffi};
use serde::Serialize;
use std::collections::BTreeMap;

/// A step, as the Swift side reads it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step<'a> {
  name: &'a str,
  script: &'a str,
  user: Option<&'a str>,
  /// Unsalted; the builder mixes in the base's digest and the rootfs ceiling.
  cache_key: &'a str,
}

impl<'a> From<&'a BuildStep> for Step<'a> {
  fn from(step: &'a BuildStep) -> Self {
    Self {
      name: &step.name,
      script: &step.script,
      user: step.user.as_deref(),
      cache_key: &step.cache_key,
    }
  }
}

/// A host directory the builder shares into the guest, as the wire spells it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Mount<'a> {
  source: String,
  destination: &'a str,
  readonly: bool,
}

impl<'a> From<&'a BuildMount> for Mount<'a> {
  fn from(mount: &'a BuildMount) -> Self {
    Self {
      source: mount.source.display().to_string(),
      destination: &mount.destination,
      readonly: mount.readonly,
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cache {
  restore: bool,
  keep: usize,
  keep_for_seconds: u64,
}

/// The plan plus what a build needs beyond it: store and boot artefacts.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Wire<'a> {
  name: &'a str,
  store_root: String,
  kernel_path: String,
  initfs_reference: &'static str,
  base: &'a str,
  tag: &'a str,
  cpus: i32,
  memory_in_bytes: u64,
  rootfs_size_in_bytes: u64,
  mounts: Vec<Mount<'a>>,
  steps: Vec<Step<'a>>,
  environment: &'a [String],
  labels: &'a BTreeMap<String, String>,
  user: Option<&'a str>,
  working_directory: Option<String>,
  ipv4_address: &'a str,
  ipv4_gateway: &'a str,
  /// The rootfs before any step.
  base_key: &'a str,
  cache: Cache,
  shell: &'a [String],
  keepalive: &'a [String],
  reclaim: bool,
}

impl<'a> Wire<'a> {
  fn new(plan: &'a BuildPlan, store: &Store) -> Self {
    Self {
      name: &plan.name,
      store_root: store.root().display().to_string(),
      kernel_path: store.kernel().display().to_string(),
      initfs_reference: INITFS_REFERENCE,
      base: &plan.base,
      tag: &plan.tag,
      cpus: plan.resources.cpus as i32,
      memory_in_bytes: plan.resources.memory_in_bytes,
      rootfs_size_in_bytes: plan.rootfs_capacity_in_bytes,
      mounts: plan.mounts.iter().map(Mount::from).collect(),
      steps: plan.steps.iter().map(Step::from).collect(),
      environment: &plan.environment,
      labels: &plan.labels,
      user: plan.user.as_deref(),
      working_directory: plan.workdir.as_ref().map(|path| path.display().to_string()),
      ipv4_address: &plan.network.ipv4_address,
      ipv4_gateway: &plan.network.ipv4_gateway,
      base_key: &plan.base_key,
      cache: Cache {
        restore: plan.cache.restore,
        keep: plan.cache.keep,
        keep_for_seconds: plan.cache.keep_for.as_secs(),
      },
      shell: &plan.shell.0,
      keepalive: &plan.keepalive,
      reclaim: plan.reclaim,
    }
  }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionWire {
  store_root: String,
  kernel_path: String,
  kernel_url: &'static str,
  kernel_in_archive: &'static str,
  initfs_reference: &'static str,
}

pub struct Builder {
  store: Store,
}

impl Builder {
  pub fn new(store: Store) -> Self {
    Self { store }
  }

  pub fn store(&self) -> &Store {
    &self.store
  }

  /// Builds the plan's image, streaming the log to stderr. Returns once it is
  /// stored under `plan.tag`.
  pub fn build(&self, plan: &BuildPlan) -> Result<(), Error> {
    let action = format!("build {}", plan.tag);
    let wire = json(&action, &Wire::new(plan, &self.store))?;

    checked(ffi::czbridge_build(&wire))
      .map(|_| ())
      .map_err(|error| Error::failed(action, error))
  }

  /// Fetches the kernel and the init image into the store if either is
  /// missing. Idempotent, and cheap when there is nothing to do.
  pub fn provision(&self) -> Result<(), Error> {
    let action = "provision the image store";
    let wire = json(
      action,
      &ProvisionWire {
        store_root: self.store.root().display().to_string(),
        kernel_path: self.store.kernel().display().to_string(),
        kernel_url: KERNEL_URL,
        kernel_in_archive: KERNEL_IN_ARCHIVE,
        initfs_reference: INITFS_REFERENCE,
      },
    )?;

    checked(ffi::czbridge_provision(&wire))
      .map(|_| ())
      .map_err(|error| Error::failed(action, error))
  }
}

fn json(action: &str, wire: &impl Serialize) -> Result<String, Error> {
  serde_json::to_string(wire).map_err(|error| Error::failed(action.to_string(), error))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{Network, Resources, Shell};

  fn plan() -> BuildPlan {
    let mut plan = BuildPlan::new(
      "builder-0123abcd",
      "docker.io/library/debian:stable-slim",
      "example/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
      Network {
        ipv4_address: "192.168.64.7/24".to_string(),
        ipv4_gateway: "192.168.64.1".to_string(),
      },
      "basekey",
    );

    plan.mounts = vec![BuildMount {
      destination: "/mnt/scripts".to_string(),
      readonly: true,
      source: "/Users/user/.cache/build/base".into(),
    }];
    plan.environment = vec!["CONFIG_DIR=/home/app/.config".to_string()];
    plan.labels = BTreeMap::from([("com.example.built-by".to_string(), "example".to_string())]);
    plan.steps = vec![
      BuildStep {
        name: "packages".to_string(),
        script: "apt-get update".to_string(),
        user: None,
        cache_key: "stepkey0".to_string(),
      },
      BuildStep {
        name: "bashrc".to_string(),
        script: "echo hook >> ~/.bashrc".to_string(),
        user: Some("app".to_string()),
        cache_key: "stepkey1".to_string(),
      },
    ];
    plan.user = Some("app".to_string());
    plan.workdir = Some("/workspace".into());

    plan
  }

  fn wire(plan: &BuildPlan) -> serde_json::Value {
    serde_json::to_value(Wire::new(plan, &Store::at("/store"))).expect("a plan should serialize")
  }

  /// Swift decodes by property name; a renamed field fails only at runtime.
  #[test]
  fn writes_the_plan_with_the_keys_swift_decodes() {
    let json = wire(&plan());

    for key in [
      "name",
      "storeRoot",
      "kernelPath",
      "initfsReference",
      "base",
      "tag",
      "cpus",
      "memoryInBytes",
      "rootfsSizeInBytes",
      "mounts",
      "steps",
      "environment",
      "labels",
      "user",
      "workingDirectory",
      "ipv4Address",
      "ipv4Gateway",
      "baseKey",
      "cache",
      "shell",
      "keepalive",
      "reclaim",
    ] {
      assert!(json.get(key).is_some(), "the plan should carry {key}: {json}");
    }

    for key in ["source", "destination", "readonly"] {
      assert!(
        json["mounts"][0].get(key).is_some(),
        "a mount should carry {key}: {json}"
      );
    }

    assert_eq!(json["labels"]["com.example.built-by"], "example");
    assert_eq!(json["steps"][0]["user"], serde_json::Value::Null);
    assert_eq!(json["steps"][1]["user"], "app");
    assert_eq!(json["steps"][1]["name"], "bashrc");
  }

  #[test]
  fn carries_the_keys_the_caller_derived() {
    let json = wire(&plan());

    assert_eq!(json["baseKey"], "basekey");
    assert_eq!(json["steps"][0]["cacheKey"], "stepkey0");
    assert_eq!(json["steps"][1]["cacheKey"], "stepkey1");
  }

  #[test]
  fn defaults_to_bash_with_a_step_failing_the_build() {
    assert_eq!(
      wire(&plan())["shell"],
      serde_json::json!(["/bin/bash", "-euo", "pipefail", "-c"])
    );
  }

  #[test]
  fn carries_the_shell_a_base_without_bash_needs() {
    let mut plan = plan();
    plan.shell = Shell(["/bin/sh", "-ec"].map(String::from).to_vec());

    assert_eq!(wire(&plan)["shell"], serde_json::json!(["/bin/sh", "-ec"]));
  }

  #[test]
  fn says_how_long_snapshots_are_kept() {
    let json = wire(&plan());

    assert_eq!(json["cache"]["restore"], true);
    assert_eq!(json["cache"]["keep"], 24);
    assert_eq!(json["cache"]["keepForSeconds"], 14 * 24 * 60 * 60);
  }

  #[test]
  fn says_when_the_cache_is_not_to_be_read() {
    let mut plan = plan();
    plan.cache.restore = false;

    assert_eq!(wire(&plan)["cache"]["restore"], false);
  }

  #[test]
  fn reclaims_unless_told_otherwise() {
    let mut plan = plan();

    assert_eq!(wire(&plan)["reclaim"], true);

    plan.reclaim = false;

    assert_eq!(wire(&plan)["reclaim"], false);
  }

  #[test]
  fn carries_the_builders_own_resources_and_ceiling() {
    let json = wire(&plan());

    assert_eq!(json["cpus"], 4);
    assert_eq!(json["memoryInBytes"], 8u64 << 30);
    assert_eq!(json["rootfsSizeInBytes"], 8u64 << 30);
  }
}
