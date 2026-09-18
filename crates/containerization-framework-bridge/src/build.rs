//! Building images.
//!
//! Pull the base, unpack it to a writable ext4 block, boot that, run each step in
//! it, export the block back to a tar, and write the tar into the store as a
//! single-layer image. All of it through Containerization, in this process, with
//! nothing to start first.
//!
//! A rebuild runs only the steps whose inputs changed: the rootfs is snapshotted
//! after each step under a `compostbin_engine::cache` key, and the next build
//! resumes from the deepest match. The image is still one layer.
//!
//! A build also removes stale builder rootfs and unreferenced blobs.

use crate::store::{KERNEL_IN_ARCHIVE, KERNEL_URL, Store};
use crate::{checked, ffi, spec};
use compostbin_engine::builder::Builder;
use compostbin_engine::cache;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{BuildPlan, BuildStep};
use serde::Serialize;

/// The rootfs the base image is unpacked into. Sparse, so this is a ceiling
/// rather than an allocation — but everything a build installs has to fit.
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

/// The plan, plus everything the plan does not say and a boot needs: where the
/// store is, what boots the builder, and what address it takes.
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
  context: Option<String>,
  steps: Vec<Step<'a>>,
  environment: &'a [String],
  user: Option<&'a str>,
  working_directory: Option<String>,
  ipv4_address: String,
  ipv4_gateway: &'static str,
  /// The rootfs before any step.
  base_key: &'a str,
  use_cache: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionWire<'a> {
  store_root: String,
  kernel_path: String,
  kernel_url: &'static str,
  kernel_in_archive: &'static str,
  initfs_reference: &'a str,
}

pub struct FrameworkBuilder {
  store: Store,
}

impl FrameworkBuilder {
  pub fn new(store: Store) -> Self {
    Self { store }
  }

  /// Puts the kernel and the init image in the store, downloading and pulling
  /// whatever is not already there.
  ///
  /// Idempotent, and cheap when there is nothing to do, so `build` can simply
  /// call it first rather than leave it to a separate command that has to be
  /// remembered.
  pub fn provision(&self) -> Result<(), EngineError> {
    let wire = ProvisionWire {
      store_root: self.store.root().display().to_string(),
      kernel_path: self.store.kernel().display().to_string(),
      kernel_url: KERNEL_URL,
      kernel_in_archive: KERNEL_IN_ARCHIVE,
      initfs_reference: self.store.initfs_reference(),
    };

    checked(ffi::compostbin_provision(&Self::json(
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
    let name = builder_name(&plan.tag);
    let action = format!("build {}", plan.tag);
    let keys = cache::keys(plan)?;
    let wire = Wire {
      store_root: self.store.root().display().to_string(),
      kernel_path: self.store.kernel().display().to_string(),
      initfs_reference: self.store.initfs_reference(),
      base: &plan.base,
      tag: &plan.tag,
      cpus: plan.resources.cpus as i32,
      memory_in_bytes: plan.resources.memory_in_bytes,
      rootfs_size_in_bytes: ROOTFS_SIZE_IN_BYTES,
      context: plan.context.as_ref().map(|path| path.display().to_string()),
      steps: plan
        .steps
        .iter()
        .zip(&keys.steps)
        .map(|(step, key)| Step::new(step, key))
        .collect(),
      environment: &plan.environment,
      user: plan.user.as_deref(),
      working_directory: plan.workdir.as_ref().map(|path| path.display().to_string()),
      ipv4_address: spec::nat_address(&name),
      ipv4_gateway: spec::NAT_GATEWAY,
      base_key: &keys.base,
      use_cache: plan.cache,
      name,
    };

    checked(ffi::compostbin_build(&Self::json(&action, &wire)?))
      .map(|_| ())
      .map_err(|error| EngineError::failed(action, error))
  }
}

/// The builder container's id, and so the name of its directory in the store.
/// Derived from the tag so two builds at once cannot share a rootfs, and stable
/// so a rebuild reuses the same directory.
fn builder_name(tag: &str) -> String {
  let slug: String = tag
    .chars()
    .map(|character| {
      if character.is_ascii_alphanumeric() {
        character
      } else {
        '-'
      }
    })
    .collect();

  format!("compostbin-build-{slug}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use compostbin_engine::model::Resources;

  fn plan() -> BuildPlan {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    plan.context = Some("/Users/user/.cache/compostbin/build/base".into());
    plan.environment = vec!["CLAUDE_CONFIG_DIR=/home/claude/.claude".to_string()];
    plan.steps = vec![
      BuildStep::root("packages", "apt-get update"),
      BuildStep::as_user("bashrc", "claude", "echo hook >> ~/.bashrc"),
    ];
    plan.user = Some("claude".to_string());
    plan.workdir = Some("/workspace".into());

    plan
  }

  fn wire(plan: &BuildPlan) -> serde_json::Value {
    let name = builder_name(&plan.tag);
    // No step names the context mount, so the nonexistent context isn't read.
    let keys = cache::keys(plan).expect("a plan with no context to read should key");

    serde_json::to_value(Wire {
      store_root: "/store".to_string(),
      kernel_path: "/store/kernels/default.kernel-arm64".to_string(),
      initfs_reference: "vminit:0",
      base: &plan.base,
      tag: &plan.tag,
      cpus: plan.resources.cpus as i32,
      memory_in_bytes: plan.resources.memory_in_bytes,
      rootfs_size_in_bytes: ROOTFS_SIZE_IN_BYTES,
      context: plan.context.as_ref().map(|path| path.display().to_string()),
      steps: plan
        .steps
        .iter()
        .zip(&keys.steps)
        .map(|(step, key)| Step::new(step, key))
        .collect(),
      environment: &plan.environment,
      user: plan.user.as_deref(),
      working_directory: plan.workdir.as_ref().map(|path| path.display().to_string()),
      ipv4_address: spec::nat_address(&name),
      ipv4_gateway: spec::NAT_GATEWAY,
      base_key: &keys.base,
      use_cache: plan.cache,
      name,
    })
    .expect("a plan should serialize")
  }

  #[test]
  fn names_a_builder_after_the_tag_it_is_building() {
    assert_eq!(
      builder_name("compostbin/base:latest"),
      "compostbin-build-compostbin-base-latest"
    );
  }

  /// The Swift side decodes by property name, so the keys are the contract. A
  /// renamed field is a runtime decode failure rather than a compile error, which
  /// is what this is here to catch.
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
      "context",
      "steps",
      "environment",
      "user",
      "workingDirectory",
      "ipv4Address",
      "ipv4Gateway",
      "baseKey",
      "useCache",
    ] {
      assert!(json.get(key).is_some(), "the plan should carry {key}: {json}");
    }

    assert_eq!(json["steps"][0]["user"], serde_json::Value::Null);
    assert_eq!(json["steps"][1]["user"], "claude");
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
