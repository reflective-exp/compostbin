//! What a caller describes to this crate: a container to boot, a process to run
//! in one, and an image to build.
//!
//! Plain data, and platform-free, so a caller compiles against it anywhere. The
//! wire these become is private to `session` and `builder`.

use crate::stdio::Stdio;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// What a VM is given. Always set by the caller: nothing here defaults it,
/// because a sensible size depends on the workload, not on the framework.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resources {
  pub cpus: u32,
  pub memory_in_bytes: u64,
}

/// A host directory shared into the guest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
  pub readonly: bool,
  pub source: PathBuf,
  pub target: PathBuf,
}

/// Which way a relayed socket is reached.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
  /// The guest connects; the listener is on the host.
  IntoGuest,
  /// The host connects; the listener is in the guest.
  OutOfGuest,
}

/// A unix socket relayed between host and guest.
///
/// Not a [`Mount`]: mounting a socket relays nothing, so Containerization
/// configures relays separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketRelay {
  pub source: PathBuf,
  pub target: PathBuf,
  /// Mode of the socket this creates. `0o666` lets an unprivileged guest user
  /// open one the guest owns as root; narrow it when that user is known.
  pub mode: u32,
  pub direction: Direction,
}

impl SocketRelay {
  /// A host socket the guest reaches, world-accessible inside the guest.
  pub fn into_guest(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
    Self {
      source: source.into(),
      target: target.into(),
      mode: 0o666,
      direction: Direction::IntoGuest,
    }
  }
}

/// The guest's only network interface: Virtualization.framework's built-in NAT.
///
/// Static, because nothing hands out leases — the guest agent sets the address
/// directly — so the caller allocates. Collisions with other guests on the
/// shared network go undetected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Network {
  /// The guest's address, as CIDR.
  pub ipv4_address: String,
  pub ipv4_gateway: String,
}

/// A container to create and start.
///
/// Mounts keep their declared order, which matters for nested paths. Sockets
/// are relayed after them; nothing nests in a socket, so that is safe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootSpec {
  /// The container's id, and its directory in the store.
  pub name: String,
  /// Registry-qualified: nothing expands `debian:stable-slim`.
  pub image: String,
  pub resources: Resources,
  /// The container's first process. It outlives every `exec`, so a workload
  /// that exits takes the container with it — pass a keepalive to hold one open.
  pub arguments: Vec<String>,
  /// `NAME=VALUE`, already resolved against wherever the caller takes values
  /// from. Nothing here reads the host environment.
  pub environment: Vec<String>,
  pub mounts: Vec<Mount>,
  pub sockets: Vec<SocketRelay>,
  pub workdir: Option<PathBuf>,
  pub network: Network,
  /// Ceiling for an image's unpacked rootfs. Sparse, so a ceiling rather than
  /// an allocation, but no container may outgrow it.
  pub rootfs_capacity_in_bytes: u64,
}

impl BootSpec {
  /// `ContainerManager`'s own default rootfs ceiling.
  pub const DEFAULT_ROOTFS_CAPACITY_IN_BYTES: u64 = 8 * 1024 * 1024 * 1024;

  pub fn new(name: impl Into<String>, image: impl Into<String>, resources: Resources, network: Network) -> Self {
    Self {
      name: name.into(),
      image: image.into(),
      resources,
      arguments: Vec::new(),
      environment: Vec::new(),
      mounts: Vec::new(),
      sockets: Vec::new(),
      workdir: None,
      network,
      rootfs_capacity_in_bytes: Self::DEFAULT_ROOTFS_CAPACITY_IN_BYTES,
    }
  }
}

/// A process to run in an already-booted container.
#[derive(Debug)]
pub struct ExecRequest {
  /// The container to run it in.
  pub name: String,
  /// Distinguishes concurrent processes in one container; a resize names it.
  pub id: String,
  pub arguments: Vec<String>,
  /// `NAME=VALUE`. Applied over the image's own, so these win.
  pub environment: Vec<String>,
  /// The guest user, as the image names it. `None` is the image's default.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
  /// `TERM` for a process on a terminal. Ignored without one.
  pub term: Option<String>,
  /// The descriptors the process runs against.
  pub stdio: Stdio,
}

impl ExecRequest {
  /// What a terminal reports itself as when the caller says nothing.
  pub const DEFAULT_TERM: &'static str = "xterm";

  pub fn new(name: impl Into<String>, id: impl Into<String>, arguments: Vec<String>, stdio: Stdio) -> Self {
    Self {
      name: name.into(),
      id: id.into(),
      arguments,
      environment: Vec::new(),
      user: None,
      workdir: None,
      term: Some(Self::DEFAULT_TERM.to_string()),
      stdio,
    }
  }
}

/// One build step: a named `RUN`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildStep {
  /// The step's label in the build log. Not part of its cache key.
  pub name: String,
  pub script: String,
  /// The guest user, as the image names it. `None` is root.
  pub user: Option<String>,
  /// The rootfs once this step has run. Opaque here: what invalidates a build
  /// is the caller's policy, so the caller derives these and this crate only
  /// stores and compares them. Salted with the base's digest and the rootfs
  /// ceiling, which only a build knows.
  pub cache_key: String,
}

/// A host directory shared into every step of a build: what a step reads in
/// place of `COPY`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildMount {
  pub destination: String,
  pub readonly: bool,
  pub source: PathBuf,
}

/// What runs a step's script, with the script appended as the final argument.
///
/// The default is `bash -euo pipefail -c`, not `sh -c`: a silent mid-step
/// failure would be baked into the image. It requires bash in the base; pass
/// `["/bin/sh", "-ec"]` for a base without one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shell(pub Vec<String>);

impl Default for Shell {
  fn default() -> Self {
    Self(
      ["/bin/bash", "-euo", "pipefail", "-c"]
        .map(String::from)
        .to_vec(),
    )
  }
}

/// How a build treats its rootfs snapshots.
///
/// Snapshots are written whether or not they are read, so a build with
/// `restore` off still leaves the next one something to resume from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachePolicy {
  /// Resume from the deepest matching snapshot. Off is `--no-cache`.
  pub restore: bool,
  /// Snapshots kept, most recently used first.
  pub keep: usize,
  /// Anything unused this long goes regardless.
  pub keep_for: Duration,
}

impl Default for CachePolicy {
  fn default() -> Self {
    Self {
      restore: true,
      keep: 24,
      keep_for: Duration::from_secs(14 * 24 * 60 * 60),
    }
  }
}

/// An image to build: a base, steps, and what the result runs as.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildPlan {
  /// The builder container's id, and its directory in the store, removed
  /// before and after the build. Distinct per concurrent build.
  pub name: String,
  /// Registry-qualified. Every step runs on top of it.
  pub base: String,
  /// What the finished image is registered as.
  pub tag: String,
  /// The builder's own resources, not those of a container run from the image.
  pub resources: Resources,
  /// Host directories shared into every step, where the caller asked for them.
  pub mounts: Vec<BuildMount>,
  pub steps: Vec<BuildStep>,
  /// `NAME=VALUE`, visible to every step and written into the image config,
  /// like `ENV`. The base's own are kept unless a name here overrides one.
  pub environment: Vec<String>,
  /// Written into the image config as its OCI labels. The base's carry over;
  /// these win on a key both set.
  pub labels: BTreeMap<String, String>,
  /// The user and directory the finished image runs as. `None` keeps the base's.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
  pub network: Network,
  /// The rootfs before any step. See [`BuildStep::cache_key`].
  pub base_key: String,
  /// Ceiling for the builder's rootfs. Every step's result must fit.
  pub rootfs_capacity_in_bytes: u64,
  pub cache: CachePolicy,
  /// What runs each step's script.
  pub shell: Shell,
  /// The builder's first process, which must outlive every step. The base's
  /// own `Cmd` would exit and take the container with it.
  pub keepalive: Vec<String>,
  /// Delete unreferenced blobs and the unpacked rootfs of removed images once
  /// the build has stored its result. Off leaves the previous build's layer,
  /// which is usually the largest thing in the store.
  pub reclaim: bool,
}

impl BuildPlan {
  /// Holds the builder open while steps run as `exec`s.
  pub fn default_keepalive() -> Vec<String> {
    ["/bin/sh", "-c", "while :; do sleep 86400; done"]
      .map(String::from)
      .to_vec()
  }

  pub fn new(
    name: impl Into<String>,
    base: impl Into<String>,
    tag: impl Into<String>,
    resources: Resources,
    network: Network,
    base_key: impl Into<String>,
  ) -> Self {
    Self {
      name: name.into(),
      base: base.into(),
      tag: tag.into(),
      resources,
      mounts: Vec::new(),
      steps: Vec::new(),
      environment: Vec::new(),
      labels: BTreeMap::new(),
      user: None,
      workdir: None,
      network,
      base_key: base_key.into(),
      rootfs_capacity_in_bytes: BootSpec::DEFAULT_ROOTFS_CAPACITY_IN_BYTES,
      cache: CachePolicy::default(),
      shell: Shell::default(),
      keepalive: Self::default_keepalive(),
      reclaim: true,
    }
  }
}
