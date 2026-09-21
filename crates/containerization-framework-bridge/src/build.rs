//! Building images.
//!
//! Pull the base, unpack it to a writable ext4 block, boot it, run each step,
//! export the block to a tar, and store that as a single-layer image — all
//! in-process through Containerization.
//!
//! The rootfs is snapshotted after each step under a `compostbin_engine::cache`
//! key; a rebuild resumes from the deepest match. The image is still one layer.
//!
//! A build also removes stale builder rootfs and unreferenced blobs.

use crate::store::{INITFS_REFERENCE, KERNEL_IN_ARCHIVE, KERNEL_URL, Store};
use crate::{checked, ffi, spec};
use compostbin_engine::builder::Builder;
use compostbin_engine::cache::{self, Keys};
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{BuildMount, BuildPlan, BuildStep};
use serde::Serialize;
use std::collections::BTreeMap;

/// Sparse, so a ceiling rather than an allocation — but every build must fit.
const ROOTFS_SIZE_IN_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// A step, as the Swift side reads it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step<'a> {
  name: &'a str,
  script: &'a str,
  user: Option<&'a str>,
  /// The rootfs after this step. The builder salts it with the base's digest.
  cache_key: &'a str,
}

impl<'a> Step<'a> {
  fn new(step: &'a BuildStep, cache_key: &'a str) -> Self {
    Self {
      name: &step.name,
      script: &step.script,
      user: step.user.as_deref(),
      cache_key,
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

impl<'a> Mount<'a> {
  fn new(mount: &'a BuildMount) -> Self {
    Self {
      source: mount.source.display().to_string(),
      destination: &mount.destination,
      readonly: mount.readonly,
    }
  }
}

/// The plan plus what a boot needs beyond it: store, boot artefacts, address.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Wire<'a> {
  name: String,
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
  ipv4_address: String,
  ipv4_gateway: &'static str,
  /// The rootfs before any step.
  base_key: &'a str,
  use_cache: bool,
}

impl<'a> Wire<'a> {
  fn new(name: String, store_root: String, kernel_path: String, plan: &'a BuildPlan, keys: &'a Keys) -> Self {
    Self {
      store_root,
      kernel_path,
      initfs_reference: INITFS_REFERENCE,
      base: &plan.base,
      tag: &plan.tag,
      cpus: plan.resources.cpus as i32,
      memory_in_bytes: plan.resources.memory_in_bytes,
      rootfs_size_in_bytes: ROOTFS_SIZE_IN_BYTES,
      mounts: plan.mounts.iter().map(Mount::new).collect(),
      steps: plan
        .steps
        .iter()
        .zip(&keys.steps)
        .map(|(step, key)| Step::new(step, key))
        .collect(),
      environment: &plan.environment,
      labels: &plan.labels,
      user: plan.user.as_deref(),
      working_directory: plan.workdir.as_ref().map(|path| path.display().to_string()),
      ipv4_address: spec::nat_address(&name),
      ipv4_gateway: spec::NAT_GATEWAY,
      base_key: &keys.base,
      use_cache: plan.cache,
      name,
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

pub struct FrameworkBuilder {
  store: Store,
}

impl FrameworkBuilder {
  pub fn new(store: Store) -> Self {
    Self { store }
  }

  /// Fetches the kernel and init image into the store if missing. Idempotent
  /// and cheap, so a caller can run it before every build.
  pub fn provision(&self) -> Result<(), EngineError> {
    let wire = ProvisionWire {
      store_root: self.store.root().display().to_string(),
      kernel_path: self.store.kernel().display().to_string(),
      kernel_url: KERNEL_URL,
      kernel_in_archive: KERNEL_IN_ARCHIVE,
      initfs_reference: INITFS_REFERENCE,
    };

    checked(ffi::czbridge_provision(&Self::json(
      "provision the image store",
      &wire,
    )?))
    .map(|_| ())
    .map_err(|error| EngineError::failed("provision the image store", error))
  }

  fn json(action: &str, wire: &impl Serialize) -> Result<String, EngineError> {
    serde_json::to_string(wire).map_err(|error| EngineError::failed(action.to_string(), error))
  }
}

impl Builder for FrameworkBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    let action = format!("build {}", plan.tag);
    let keys = cache::keys(plan)?;
    let name = builder_name().map_err(|error| EngineError::failed(&action, error))?;
    let wire = Wire::new(
      name,
      self.store.root().display().to_string(),
      self.store.kernel().display().to_string(),
      plan,
      &keys,
    );

    checked(ffi::czbridge_build(&Self::json(&action, &wire)?))
      .map(|_| ())
      .map_err(|error| EngineError::failed(action, error))
  }
}

/// The builder container's id (and store directory). Random, so concurrent
/// builds never share a rootfs; short, as Containerization caps ids at 64.
fn builder_name() -> Result<String, getrandom::Error> {
  let mut bytes = [0u8; 4];
  getrandom::fill(&mut bytes)?;

  Ok(format!("cb-builder-{:08x}", u32::from_be_bytes(bytes)))
}

#[cfg(test)]
mod tests {
  use super::*;
  use compostbin_engine::model::Resources;

  fn plan() -> BuildPlan {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "example/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    plan.mounts = vec![BuildMount {
      destination: "/mnt/scripts".to_string(),
      readonly: true,
      source: "/Users/user/.cache/containerization/build/base".into(),
    }];
    plan.environment = vec!["CONFIG_DIR=/home/app/.config".to_string()];
    plan.labels = BTreeMap::from([("com.example.built-by".to_string(), "example".to_string())]);
    plan.steps = vec![
      BuildStep::root("packages", "apt-get update"),
      BuildStep::as_user("bashrc", "app", "echo hook >> ~/.bashrc"),
    ];
    plan.user = Some("app".to_string());
    plan.workdir = Some("/workspace".into());

    plan
  }

  fn wire(plan: &BuildPlan) -> serde_json::Value {
    // No step names the context mount, so the nonexistent context isn't read.
    let keys = cache::keys(plan).expect("a plan with no context to read should key");

    serde_json::to_value(Wire::new(
      "cb-0123abcd".to_string(),
      "/store".to_string(),
      "/store/kernels/default.kernel-arm64".to_string(),
      plan,
      &keys,
    ))
    .expect("a plan should serialize")
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
      "useCache",
    ] {
      assert!(json.get(key).is_some(), "the plan should carry {key}: {json}");
    }

    assert_eq!(json["labels"]["com.example.built-by"], "example");
    assert_eq!(json["steps"][0]["user"], serde_json::Value::Null);
    assert_eq!(json["steps"][1]["user"], "app");
    assert_eq!(json["steps"][1]["name"], "bashrc");
    assert!(json["steps"][0]["cacheKey"].is_string());
  }

  #[test]
  fn carries_a_cache_key_for_the_base_and_for_every_step() {
    let json = wire(&plan());
    let base = json["baseKey"].as_str().expect("a base key");
    let first = json["steps"][0]["cacheKey"].as_str().expect("a step key");
    let second = json["steps"][1]["cacheKey"].as_str().expect("a step key");

    assert_ne!(base, first);
    assert_ne!(first, second);
  }

  #[test]
  fn says_when_the_cache_is_not_to_be_trusted() {
    let mut plan = plan();

    assert_eq!(wire(&plan)["useCache"], true);

    plan.cache = false;

    assert_eq!(wire(&plan)["useCache"], false);
  }

  #[test]
  fn carries_the_builders_own_resources() {
    let json = wire(&plan());

    assert_eq!(json["cpus"], 4);
    assert_eq!(json["memoryInBytes"], 8u64 << 30);
  }
}
