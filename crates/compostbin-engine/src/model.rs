use std::fmt;
use std::path::PathBuf;

/// One build step: a named `RUN`. The name is what the build log shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildStep {
  pub name: String,
  pub script: String,
  /// The guest user, as the image names it. `None` is root.
  pub user: Option<String>,
}

impl BuildStep {
  pub fn root(name: &str, script: impl Into<String>) -> Self {
    Self {
      name: name.to_string(),
      script: script.into(),
      user: None,
    }
  }

  pub fn as_user(name: &str, user: &str, script: impl Into<String>) -> Self {
    Self {
      name: name.to_string(),
      script: script.into(),
      user: Some(user.to_string()),
    }
  }
}

/// An image to build: a base, steps, and what the result runs as.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildPlan {
  /// Registry-qualified: nothing expands `debian:stable-slim` to
  /// `docker.io/library/…`.
  pub base: String,
  /// Whether to start from cached snapshots. Off still writes them.
  pub cache: bool,
  /// A read-only host directory steps install from (the `COPY` equivalent).
  pub context: Option<PathBuf>,
  /// `NAME=VALUE`, visible to every step and written into the image config.
  pub environment: Vec<String>,
  /// The builder's own resources, not a session's.
  pub resources: Resources,
  pub steps: Vec<BuildStep>,
  /// What the built image is registered as.
  pub tag: String,
  /// The user and directory the built image runs as.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
}

impl BuildPlan {
  pub fn new(base: impl Into<String>, tag: impl Into<String>, resources: Resources) -> Self {
    Self {
      base: base.into(),
      cache: true,
      context: None,
      environment: Vec::new(),
      resources,
      steps: Vec::new(),
      tag: tag.into(),
      user: None,
      workdir: None,
    }
  }
}

/// What a VM is given. Always set by the caller, never defaulted by an engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resources {
  pub cpus: u32,
  pub memory_in_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvVar {
  /// The host's value; dropped if unset, so the guest can tell unset from empty.
  Inherit(String),
  Set {
    name: String,
    value: String,
  },
}

/// A process to run in an already-running container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecSpec {
  pub arguments: Vec<String>,
  pub env: Vec<EnvVar>,
  /// Whether the process reads the caller's stdin.
  pub interactive: bool,
  /// The container to run it in.
  pub name: String,
  /// Whether it gets the caller's terminal. Only the caller's own stdio says
  /// whether there is one, so an engine may run without it.
  pub tty: bool,
  /// The guest user, as the image names it. `None` is the image's default.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
  pub readonly: bool,
  pub source: PathBuf,
  pub target: PathBuf,
}

/// `source:target[:ro]`, as `doctor`'s drift report shows it.
impl fmt::Display for Mount {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}:{}", self.source.display(), self.target.display())?;

    if self.readonly {
      formatter.write_str(":ro")?;
    }

    Ok(())
  }
}

/// A host unix socket relayed into the guest. Not a `Mount`: mounting a socket
/// relays nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketRelay {
  pub source: PathBuf,
  pub target: PathBuf,
}

/// A container to create and start. Mounts keep declared order, which matters
/// for nested paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSpec {
  pub arguments: Vec<String>,
  pub env: Vec<EnvVar>,
  pub image: String,
  pub mounts: Vec<Mount>,
  pub name: String,
  pub resources: Resources,
  /// Relayed after the mounts; nothing nests in a socket, so that's safe.
  pub sockets: Vec<SocketRelay>,
  pub workdir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_the_user_a_step_runs_as_only_when_it_is_not_root() {
    assert_eq!(BuildStep::root("packages", "apt-get update").user, None);
    assert_eq!(
      BuildStep::as_user("bashrc", "claude", "echo >> ~/.bashrc").user,
      Some("claude".to_string())
    );
  }

  #[test]
  fn reads_a_mount_as_its_two_paths_and_its_mode() {
    let mount = Mount {
      readonly: false,
      source: "/Users/user/workspace".into(),
      target: "/workspace".into(),
    };

    assert_eq!(mount.to_string(), "/Users/user/workspace:/workspace");
    assert_eq!(
      Mount {
        readonly: true,
        ..mount.clone()
      }
      .to_string(),
      "/Users/user/workspace:/workspace:ro"
    );
  }
}
