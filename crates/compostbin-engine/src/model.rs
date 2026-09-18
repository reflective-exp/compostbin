use std::fmt;
use std::path::PathBuf;

/// One step of a build: a shell script, and who runs it.
///
/// A `RUN` line, in other words — but named, because the name is what the build
/// log shows, and a build whose log is a wall of shell is a build nobody reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildStep {
  pub name: String,
  pub script: String,
  /// The guest user, as the image being built names it. `None` is root, which is
  /// what installing packages needs.
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

/// An image to build: a base, a sequence of steps, and what the result runs as.
///
/// Structured, because what composes it is structured: a project's additions come
/// from manifest fields, and a builder that runs the steps itself has no reason to
/// be handed a script to parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildPlan {
  /// The image the steps run on top of, registry-qualified: nothing resolves a
  /// bare `debian:stable-slim` into `docker.io/library/…`.
  pub base: String,
  /// A host directory the steps may read, mounted read-only. This is what a
  /// `COPY` is: a step installs out of it.
  pub context: Option<PathBuf>,
  pub cpus: Option<u32>,
  /// `NAME=VALUE`, visible to every step and written into the image's config —
  /// `ENV`, in both of its meanings.
  pub environment: Vec<String>,
  /// What the builder runs with, not what a session does.
  pub memory: Option<String>,
  pub steps: Vec<BuildStep>,
  /// What the built image is registered as.
  pub tag: String,
  /// The user and directory the built image runs as.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
}

impl BuildPlan {
  pub fn new(base: impl Into<String>, tag: impl Into<String>) -> Self {
    Self {
      base: base.into(),
      context: None,
      cpus: None,
      environment: Vec::new(),
      memory: None,
      steps: Vec::new(),
      tag: tag.into(),
      user: None,
      workdir: None,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvVar {
  /// Carried into the guest with whatever value the host has. Dropped when the
  /// host does not set it, so the guest can tell unset from empty.
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
  /// Whether the process gets the caller's stdin.
  pub interactive: bool,
  /// The container to run it in.
  pub name: String,
  /// Whether it gets a terminal.
  pub tty: bool,
  pub workdir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
  pub readonly: bool,
  pub source: PathBuf,
  pub target: PathBuf,
}

/// `source:target`, and `:ro` when it is read-only. How a mount reads in
/// `doctor`'s drift report, which is the one place a mount has to be a sentence.
impl fmt::Display for Mount {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}:{}", self.source.display(), self.target.display())?;

    if self.readonly {
      formatter.write_str(":ro")?;
    }

    Ok(())
  }
}

/// A host unix socket carried into the guest, rather than mounted there.
///
/// Its own field rather than a `Mount` that happens to point at a socket: a relay
/// is configuration, not a filesystem, and mounting a socket as one relays
/// nothing and is not much of a mount either.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketRelay {
  pub source: PathBuf,
  pub target: PathBuf,
}

/// A container to create and start. Mounts keep their declared order, which
/// matters when one mounted path nests inside another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSpec {
  pub arguments: Vec<String>,
  pub cpus: Option<u32>,
  /// Whether the container outlives the call that started it.
  pub detach: bool,
  pub env: Vec<EnvVar>,
  pub image: String,
  pub memory: Option<String>,
  pub mounts: Vec<Mount>,
  pub name: String,
  /// Relayed after the mounts. Nothing nests inside a socket, so the order
  /// between the two groups does not matter the way it does within `mounts`.
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
