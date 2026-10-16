//! The built image is always one layer; the cache is rootfs block snapshots, not layers.

use super::cache::Keys;
use super::oci::{self, Built, ImageConfig};
use super::progress::Meter;
use super::provision::initfs_mount;
use super::snapshots::{Salted, Snapshots, short};
use super::store::Store;
use super::unpacked::{ROOTFS_SIZE_IN_BYTES, Unpacked, ext4_root};
use super::{nat, note, spec};
use crate::error::EngineError;
use crate::model::BuildPlan;
use containerization_framework::containerization::container::container_manager::CreateWithRootfsOptions;
use containerization_framework::containerization::container::{
  ContainerManager, FilesystemOperation, LinuxContainer, linux_container,
};
use containerization_framework::containerization::image::image_store::PullOptions;
use containerization_framework::containerization::image::{self, EXT4Unpacker, Image, ImageStore};
use containerization_framework::containerization::process::LinuxProcessConfiguration;
use containerization_framework::containerization::vm::{Kernel, SystemPlatform};
use containerization_framework::containerization_error::Code;
use containerization_framework::containerization_ext4::ext4::EXT4Reader;
use containerization_framework::containerization_oci::content::LocalContentStore;
use containerization_framework::containerization_oci::image::{Descriptor, Platform};
use containerization_framework::containerization_oci::runtime::User;
use serde_json::Value;
use std::path::Path;

/// Not `sh -c`, so a failing command fails the step.
const SHELL: [&str; 4] = ["/bin/bash", "-euo", "pipefail", "-c"];

/// Must outlive every step; the base's own `Cmd` could exit and stop the container.
const KEEPALIVE: [&str; 3] = ["/bin/sh", "-c", "while :; do sleep 86400; done"];

/// Marks a builder's directory in `containers`; holds the owning build's pid.
const BUILDER_MARKER: &str = ".builder";

pub fn build(store: &Store, plan: &BuildPlan, keys: Keys, name: &str) -> Result<(), EngineError> {
  let (content, images) = store.content()?;
  let platform = Platform::current()?;

  // Pulled first because its digest salts every cache key.
  let base = pulled(&images, &plan.base)?;
  let base_config = oci::image_config(&base, &platform)?;
  let environment = oci::merge(oci::strings(&base_config, "Env"), &plan.environment);

  let snapshots = Snapshots::at(store.build_cache());
  let keys = Salted::new(
    keys,
    &base.digest(),
    ROOTFS_SIZE_IN_BYTES,
    plan.user.as_deref(),
    plan.workdir.as_deref(),
  );

  if plan.cache
    && let Some(descriptor) = snapshots.image(&keys.image)
    && oci::holds(&content, &descriptor)
  {
    note(&format!("{} is already built", plan.tag));
    images.create(&image::Description::new(&plan.tag, descriptor))?;
    snapshots.evict();

    return Ok(());
  }

  let directory = store.container_dir(name);
  let rootfs = directory.join("rootfs.ext4");
  let failed = |error: std::io::Error| EngineError::failed(format!("build {}", plan.tag), error);

  let _ = std::fs::remove_dir_all(&directory);
  sweep_builders(store, name);
  std::fs::create_dir_all(&directory).map_err(failed)?;
  let _ = std::fs::write(directory.join(BUILDER_MARKER), format!("{}\n", std::process::id()));

  let start = prepare(&rootfs, plan, &keys, &snapshots, &base, &platform)?;

  if start < plan.steps.len() {
    let builder = Builder {
      store,
      images: &images,
      base: &base,
      plan,
      name,
      rootfs: &rootfs,
      environment: &environment,
    };

    builder.run(start, &keys, &snapshots)?;
  }

  let descriptor = ingest(&content, &rootfs, plan, base_config, environment, platform)?;

  images.create(&image::Description::new(&plan.tag, descriptor.clone()))?;
  snapshots.save_image(&descriptor, &keys.image);

  // Not `ContainerManager::delete`: a fully cached build has no manager.
  let _ = std::fs::remove_dir_all(&directory);

  reclaim(store, &images);
  snapshots.evict();

  Ok(())
}

/// Returns the index of the first step to run.
fn prepare(
  rootfs: &Path,
  plan: &BuildPlan,
  keys: &Salted,
  snapshots: &Snapshots,
  base: &Image,
  platform: &Platform,
) -> Result<usize, EngineError> {
  let restore = |key: &str| {
    snapshots
      .restore(key, rootfs)
      .map_err(|error| EngineError::failed(format!("restore the cached rootfs of {}", plan.tag), error))
  };

  if plan.cache {
    if let Some(index) = (0..plan.steps.len())
      .rev()
      .find(|index| snapshots.holds_rootfs(&keys.steps[*index]))
    {
      note(&format!("cached through {}", plan.steps[index].name));
      restore(&keys.steps[index])?;

      return Ok(index + 1);
    }

    if snapshots.holds_rootfs(&keys.base) {
      restore(&keys.base)?;

      return Ok(0);
    }
  }

  note(&format!("unpacking {}", plan.base));
  EXT4Unpacker::new(ROOTFS_SIZE_IN_BYTES, None).unpack(base, platform, rootfs, None)?;
  snapshots.save_rootfs(rootfs, &keys.base);

  Ok(0)
}

/// Not `get(_, true)`, which pulls without reporting progress.
fn pulled(images: &ImageStore, reference: &str) -> Result<Image, EngineError> {
  match images.get(reference, false) {
    Err(error) if error.is_code(Code::NotFound) => {
      note(&format!("pulling {reference}"));
      let meter = Meter::new();
      let options = PullOptions {
        progress: meter.handler(),
        ..Default::default()
      };

      Ok(images.pull(reference, options)?)
    }
    image => Ok(image?),
  }
}

struct Builder<'a> {
  store: &'a Store,
  images: &'a ImageStore,
  base: &'a Image,
  plan: &'a BuildPlan,
  name: &'a str,
  rootfs: &'a Path,
  environment: &'a [String],
}

impl Builder<'_> {
  fn run(&self, start: usize, keys: &Salted, snapshots: &Snapshots) -> Result<(), EngineError> {
    let kernel = Kernel::new(self.store.kernel(), SystemPlatform::LINUX_ARM);
    let mut manager = ContainerManager::new(&kernel, &initfs_mount(self.store), self.images, Default::default())?;
    let options = CreateWithRootfsOptions {
      networking: false,
      vm: spec::vm(self.plan.resources),
      ..Default::default()
    };
    let container = manager.create_with_rootfs(
      self.name,
      self.base,
      ext4_root(self.rootfs),
      options,
      self.configuration(),
    )?;

    container.create()?;
    container.start()?;

    let ran = (start..self.plan.steps.len()).try_for_each(|index| {
      self.step(&container, index)?;
      snapshot(&container, self.rootfs, snapshots, &keys.steps[index])
    });

    if ran.is_err() {
      // Left for inspection; the next build sweeps it.
      let _ = container.stop();
      return ran;
    }

    container.stop()?;

    Ok(())
  }

  fn configuration(
    &self,
  ) -> impl FnOnce(&mut linux_container::Configuration) -> Result<(), containerization_framework::Error> + Send + 'static
  {
    let name = self.name.to_string();
    let resources = self.plan.resources;
    let environment = self.environment.to_vec();
    let mounts: Vec<_> = self
      .plan
      .mounts
      .iter()
      .map(|mount| spec::share(&mount.source, mount.destination.clone(), true))
      .collect();

    move |configuration: &mut linux_container::Configuration| {
      configuration.cpus = resources.cpus;
      configuration.memory_in_bytes = resources.memory_in_bytes;
      configuration.process.arguments = KEEPALIVE.map(String::from).to_vec();
      configuration.process.user = User::default();
      configuration.process.working_directory = "/".to_string();
      configuration.process.environment_variables = environment;
      configuration.mounts.extend(mounts);
      configuration.interfaces = vec![nat::interface(&name)?];
      configuration.dns = Some(nat::dns());
      Ok(())
    }
  }

  fn step(&self, container: &LinuxContainer, index: usize) -> Result<(), EngineError> {
    let step = &self.plan.steps[index];

    note(&format!("--> {}", step.name));

    let process = container.exec(
      &format!("build-{index}"),
      LinuxProcessConfiguration {
        arguments: SHELL
          .iter()
          .map(|argument| argument.to_string())
          .chain([step.script.clone()])
          .collect(),
        environment_variables: self.environment.to_vec(),
        working_directory: "/".to_string(),
        user: user(step.user.as_deref()),
        stdout: Some(libc::STDERR_FILENO),
        stderr: Some(libc::STDERR_FILENO),
        ..Default::default()
      },
    )?;

    process.start()?;
    let status = process.wait(None)?;
    let _ = process.delete();

    if status.exit_code != 0 {
      return Err(EngineError::failed(
        format!("build {}", self.plan.tag),
        format!("step {} exited with {}", step.name, status.exit_code),
      ));
    }

    Ok(())
  }
}

/// Freezing flushes guest writes so the clone is consistent; trimming first keeps freed blocks out of it.
fn snapshot(container: &LinuxContainer, rootfs: &Path, snapshots: &Snapshots, key: &str) -> Result<(), EngineError> {
  let _ = container.filesystem_operation(FilesystemOperation::Trim, "/");

  if let Err(error) = container.filesystem_operation(FilesystemOperation::Freeze, "/") {
    note(&format!("could not cache the rootfs for {}: {error}", short(key)));
    return Ok(());
  }

  snapshots.save_rootfs(rootfs, key);
  container.filesystem_operation(FilesystemOperation::Thaw, "/")?;

  Ok(())
}

/// `None` is root.
pub fn user(name: Option<&str>) -> User {
  User {
    username: name.unwrap_or_default().to_string(),
    ..User::default()
  }
}

fn ingest(
  content: &LocalContentStore,
  rootfs: &Path,
  plan: &BuildPlan,
  base: ImageConfig,
  environment: Vec<String>,
  platform: Platform,
) -> Result<Descriptor, EngineError> {
  let layer = rootfs.with_file_name("layer.tar");
  let _ = std::fs::remove_file(&layer);

  EXT4Reader::new(rootfs)?.export(&layer)?;

  let stored = oci::ingest(
    content,
    Built {
      layer: layer.clone(),
      config: config(plan, base, environment),
      platform,
    },
  );
  let _ = std::fs::remove_file(&layer);

  stored
}

fn config(plan: &BuildPlan, mut config: ImageConfig, environment: Vec<String>) -> ImageConfig {
  config.insert("Env".to_string(), environment.into());

  if let Some(user) = &plan.user {
    config.insert("User".to_string(), user.clone().into());
  }

  if let Some(workdir) = &plan.workdir {
    config.insert("WorkingDir".to_string(), workdir.display().to_string().into());
  }

  let labels = config.entry("Labels").or_insert(Value::Null);

  if !labels.is_object() {
    *labels = Value::Object(Default::default());
  }

  if let Value::Object(labels) = labels {
    for (key, value) in &plan.labels {
      labels.insert(key.clone(), value.clone().into());
    }
  }

  config
}

/// Must run after the re-tag, since until then this build's own blobs are unreferenced.
fn reclaim(store: &Store, images: &ImageStore) {
  if let Ok(listed) = images.list() {
    Unpacked::at(store.unpacked()).evict(listed.iter().map(Image::digest));
  }

  match images.clean_up_orphaned_blobs() {
    Ok((deleted, _)) if deleted.is_empty() => {}
    Ok((deleted, freed)) => note(&format!(
      "reclaimed {} MB from {} unreferenced blob{}",
      freed >> 20,
      deleted.len(),
      if deleted.len() == 1 { "" } else { "s" }
    )),
    Err(error) => note(&format!("could not reclaim unreferenced blobs: {error}")),
  }
}

/// Only removes marked directories whose owning pid is gone; sessions and other builds share `containers`.
fn sweep_builders(store: &Store, current: &str) {
  let Ok(directories) = std::fs::read_dir(store.containers()) else {
    return;
  };

  for directory in directories.flatten().map(|entry| entry.path()) {
    if directory.file_name().is_some_and(|name| name == current) {
      continue;
    }

    let Ok(owner) = std::fs::read_to_string(directory.join(BUILDER_MARKER)) else {
      continue;
    };

    // SAFETY: signal 0 only checks whether the pid exists.
    if let Ok(pid) = owner.trim().parse::<libc::pid_t>()
      && unsafe { libc::kill(pid, 0) } == 0
    {
      continue;
    }

    if let Some(name) = directory.file_name() {
      note(&format!("removing the rootfs left by {}", name.to_string_lossy()));
    }

    let _ = std::fs::remove_dir_all(&directory);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::Resources;
  use std::collections::BTreeMap;
  use std::path::PathBuf;

  fn plan() -> BuildPlan {
    let mut plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    plan.labels = BTreeMap::from([("dev.compostbin.built-by".to_string(), "0.12.0".to_string())]);
    plan.user = Some("claude".to_string());
    plan.workdir = Some(PathBuf::from("/workspace"));

    plan
  }

  #[test]
  fn writes_the_plans_settings_over_the_bases_config() {
    let base: ImageConfig =
      serde_json::from_str(r#"{"User":"root","Cmd":["bash"],"Env":["PATH=/bin"],"Labels":{"maintainer":"debian"}}"#)
        .expect("a config");

    let config = config(&plan(), base, vec!["PATH=/bin".to_string(), "LANG=C".to_string()]);

    assert_eq!(config["User"], "claude");
    assert_eq!(config["WorkingDir"], "/workspace");
    assert_eq!(
      oci::strings(&config, "Cmd"),
      ["bash"],
      "the base's command carries over"
    );
    assert_eq!(oci::strings(&config, "Env"), ["PATH=/bin", "LANG=C"]);
    assert_eq!(config["Labels"]["maintainer"], "debian");
    assert_eq!(config["Labels"]["dev.compostbin.built-by"], "0.12.0");
  }

  #[test]
  fn keeps_the_bases_user_when_the_plan_names_none() {
    let base: ImageConfig = serde_json::from_str(r#"{"User":"nobody","Labels":null}"#).expect("a config");
    let mut plan = plan();
    plan.user = None;

    let config = config(&plan, base, Vec::new());

    assert_eq!(config["User"], "nobody");
    assert_eq!(config["Labels"]["dev.compostbin.built-by"], "0.12.0");
  }

  #[test]
  fn runs_a_step_as_root_unless_it_names_a_user() {
    assert_eq!(user(None), User::default());
    assert_eq!(user(Some("claude")).username, "claude");
  }

  #[test]
  fn sweeps_only_builders_whose_build_has_gone() {
    let root = tempfile::tempdir().expect("a temp dir");
    let store = Store::at(root.path());
    let directory = |name: &str, owner: Option<String>| {
      let path = store.container_dir(name);
      std::fs::create_dir_all(&path).expect("a container directory");

      if let Some(owner) = owner {
        std::fs::write(path.join(BUILDER_MARKER), owner).expect("a marker");
      }

      path
    };

    let session = directory("session-cb", None);
    let live = directory("cb-builder-live", Some(format!("{}\n", std::process::id())));
    let dead = directory("cb-builder-dead", Some(format!("{}\n", libc::pid_t::MAX)));
    let current = directory("cb-builder-current", Some("1\n".to_string()));

    sweep_builders(&store, "cb-builder-current");

    assert!(session.exists(), "a session's directory");
    assert!(live.exists(), "a build still running");
    assert!(!dead.exists(), "a build that died");
    assert!(current.exists(), "this build's own");
  }
}
