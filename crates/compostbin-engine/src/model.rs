use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildStep {
  pub name: String,
  pub script: String,
  /// `None` is root.
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

/// Mounted read-only into every build step.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildMount {
  pub destination: String,
  pub source: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildPlan {
  /// Must be registry-qualified; short names like `debian:stable-slim` are not expanded.
  pub base: String,
  /// Whether to start from cached snapshots; snapshots are written either way.
  pub cache: bool,
  /// A step that names a mount is cache-keyed on its contents.
  pub mounts: Vec<BuildMount>,
  /// `NAME=VALUE`, also written into the image config.
  pub environment: Vec<String>,
  pub labels: BTreeMap<String, String>,
  pub resources: Resources,
  pub steps: Vec<BuildStep>,
  pub tag: String,
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
}

impl BuildPlan {
  pub fn new(base: impl Into<String>, tag: impl Into<String>, resources: Resources) -> Self {
    Self {
      base: base.into(),
      cache: true,
      environment: Vec::new(),
      labels: BTreeMap::new(),
      mounts: Vec::new(),
      resources,
      steps: Vec::new(),
      tag: tag.into(),
      user: None,
      workdir: None,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resources {
  pub cpus: u32,
  pub memory_in_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvVar {
  /// The host's value; omitted entirely if unset on the host.
  Inherit(String),
  Set {
    name: String,
    value: String,
  },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecSpec {
  pub arguments: Vec<String>,
  pub env: Vec<EnvVar>,
  pub interactive: bool,
  pub name: String,
  /// An engine may run without a terminal when the caller's stdio is not one.
  pub tty: bool,
  /// `None` is the image's default user.
  pub user: Option<String>,
  pub workdir: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mount {
  pub readonly: bool,
  pub source: PathBuf,
  pub target: PathBuf,
}

impl fmt::Display for Mount {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "{}:{}", self.source.display(), self.target.display())?;

    if self.readonly {
      formatter.write_str(":ro")?;
    }

    Ok(())
  }
}

/// `mounts` are applied in order, which matters for nested paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSpec {
  pub arguments: Vec<String>,
  pub env: Vec<EnvVar>,
  pub image: String,
  pub mounts: Vec<Mount>,
  pub name: String,
  pub resources: Resources,
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
