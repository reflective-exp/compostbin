use std::path::PathBuf;

/// A `container build` invocation. The Dockerfile is not named: it is written
/// into `context` as `Dockerfile`, which is where the CLI looks by default.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildSpec {
  pub context: PathBuf,
  pub memory: Option<String>,
  pub tag: String,
}

impl BuildSpec {
  pub fn to_argv(&self) -> Vec<String> {
    let mut argv = vec!["build".to_string()];

    if let Some(memory) = &self.memory {
      argv.push("--memory".to_string());
      argv.push(memory.clone());
    }

    argv.push("--tag".to_string());
    argv.push(self.tag.clone());
    argv.push(self.context.display().to_string());

    argv
  }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EnvVar {
  /// `--env NAME`, inheriting the value from the host environment.
  Inherit(String),
  /// `--env NAME=VALUE`.
  Set { name: String, value: String },
}

impl EnvVar {
  fn to_argument(&self) -> String {
    match self {
      Self::Inherit(name) => name.clone(),
      Self::Set { name, value } => format!("{name}={value}"),
    }
  }
}

/// A `container exec` invocation against an already-running container.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecSpec {
  pub arguments: Vec<String>,
  pub env: Vec<EnvVar>,
  pub interactive: bool,
  pub name: String,
  pub tty: bool,
  pub workdir: Option<PathBuf>,
}

impl ExecSpec {
  pub fn to_argv(&self) -> Vec<String> {
    let mut argv = vec!["exec".to_string()];

    for variable in &self.env {
      argv.push("--env".to_string());
      argv.push(variable.to_argument());
    }

    if self.interactive {
      argv.push("--interactive".to_string());
    }

    if self.tty {
      argv.push("--tty".to_string());
    }

    if let Some(workdir) = &self.workdir {
      argv.push("--workdir".to_string());
      argv.push(workdir.display().to_string());
    }

    argv.push(self.name.clone());
    argv.extend(self.arguments.iter().cloned());

    argv
  }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
  pub readonly: bool,
  pub source: PathBuf,
  pub target: PathBuf,
}

impl Mount {
  fn to_argument(&self) -> String {
    let source = self.source.display();
    let target = self.target.display();
    if self.readonly {
      format!("{source}:{target}:ro")
    } else {
      format!("{source}:{target}")
    }
  }
}

/// A `container run` invocation. Mounts keep their declared order, which matters
/// when one mounted path nests inside another.
#[derive(Clone, Debug, PartialEq)]
pub struct RunSpec {
  pub arguments: Vec<String>,
  pub cpus: Option<u32>,
  pub detach: bool,
  pub env: Vec<EnvVar>,
  pub image: String,
  pub memory: Option<String>,
  pub mounts: Vec<Mount>,
  pub name: String,
  pub workdir: Option<PathBuf>,
}

impl RunSpec {
  pub fn to_argv(&self) -> Vec<String> {
    let mut argv = vec!["run".to_string()];

    if let Some(cpus) = self.cpus {
      argv.push("--cpus".to_string());
      argv.push(cpus.to_string());
    }

    if self.detach {
      argv.push("--detach".to_string());
    }

    for variable in &self.env {
      argv.push("--env".to_string());
      argv.push(variable.to_argument());
    }

    if let Some(memory) = &self.memory {
      argv.push("--memory".to_string());
      argv.push(memory.clone());
    }

    argv.push("--name".to_string());
    argv.push(self.name.clone());

    for mount in &self.mounts {
      argv.push("--volume".to_string());
      argv.push(mount.to_argument());
    }

    if let Some(workdir) = &self.workdir {
      argv.push("--workdir".to_string());
      argv.push(workdir.display().to_string());
    }

    argv.push(self.image.clone());
    argv.extend(self.arguments.iter().cloned());

    argv
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn builds_build_argv() {
    let spec = BuildSpec {
      context: "/Users/sax/.cache/compostbin/build".into(),
      memory: Some("8G".to_string()),
      tag: "compostbin/base:latest".to_string(),
    };

    assert_eq!(
      spec.to_argv(),
      [
        "build",
        "--memory",
        "8G",
        "--tag",
        "compostbin/base:latest",
        "/Users/sax/.cache/compostbin/build",
      ]
    );
  }

  #[test]
  fn builds_build_argv_without_a_memory_limit() {
    let spec = BuildSpec {
      context: "/tmp/context".into(),
      memory: None,
      tag: "base:latest".to_string(),
    };

    assert_eq!(spec.to_argv(), ["build", "--tag", "base:latest", "/tmp/context"]);
  }

  #[test]
  fn builds_exec_argv() {
    let spec = ExecSpec {
      arguments: vec!["claude".to_string(), "--continue".to_string()],
      env: vec![EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      }],
      interactive: true,
      name: "compostbin-compostbin".to_string(),
      tty: true,
      workdir: Some("/Users/sax/workspace/compostbin".into()),
    };

    assert_eq!(
      spec.to_argv(),
      [
        "exec",
        "--env",
        "IS_SANDBOX=1",
        "--interactive",
        "--tty",
        "--workdir",
        "/Users/sax/workspace/compostbin",
        "compostbin-compostbin",
        "claude",
        "--continue",
      ]
    );
  }

  #[test]
  fn builds_non_interactive_exec_argv() {
    let spec = ExecSpec {
      arguments: vec!["true".to_string()],
      env: Vec::new(),
      interactive: false,
      name: "compostbin-compostbin".to_string(),
      tty: false,
      workdir: None,
    };

    assert_eq!(spec.to_argv(), ["exec", "compostbin-compostbin", "true"]);
  }

  #[test]
  fn builds_run_argv() {
    let spec = RunSpec {
      arguments: vec!["claude".to_string(), "--continue".to_string()],
      cpus: Some(4),
      detach: true,
      env: vec![
        EnvVar::Inherit("ANTHROPIC_API_KEY".to_string()),
        EnvVar::Set {
          name: "IS_SANDBOX".to_string(),
          value: "1".to_string(),
        },
      ],
      image: "compostbin/base:latest".to_string(),
      memory: Some("8G".to_string()),
      mounts: vec![
        Mount {
          readonly: false,
          source: "/Users/sax/workspace".into(),
          target: "/Users/sax/workspace".into(),
        },
        Mount {
          readonly: true,
          source: "/Users/sax/.cargo/registry".into(),
          target: "/Users/sax/.cargo/registry".into(),
        },
      ],
      name: "compostbin-compostbin".to_string(),
      workdir: Some("/Users/sax/workspace/compostbin".into()),
    };

    assert_eq!(
      spec.to_argv(),
      [
        "run",
        "--cpus",
        "4",
        "--detach",
        "--env",
        "ANTHROPIC_API_KEY",
        "--env",
        "IS_SANDBOX=1",
        "--memory",
        "8G",
        "--name",
        "compostbin-compostbin",
        "--volume",
        "/Users/sax/workspace:/Users/sax/workspace",
        "--volume",
        "/Users/sax/.cargo/registry:/Users/sax/.cargo/registry:ro",
        "--workdir",
        "/Users/sax/workspace/compostbin",
        "compostbin/base:latest",
        "claude",
        "--continue",
      ]
    );
  }
}
