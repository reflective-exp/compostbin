use crate::host::{Forward, GUEST_SPOOL_TARGET};
use crate::image::GUEST_PORTS_NAME;
use crate::session::briefing::MANAGED_SETTINGS_TARGET;
use crate::session::{Process, Session};
use compostbin_engine::model::{EnvVar, ExecSpec, Mount, Resources, RunSpec};
use std::path::PathBuf;

pub const CLAUDE_HOME_TARGET: &str = "/home/claude/.claude";
const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "infinity"];
/// Names no real compositor; it only has to be set.
pub const CLIPBOARD_DISPLAY: &str = "compostbin-clipboard";

impl Session {
  /// Order matters: a nested mount must come after its parent.
  pub fn mounts(&self) -> Vec<Mount> {
    let mut mounts: Vec<Mount> = self
      .workspace()
      .entries()
      .iter()
      .map(|entry| Mount {
        readonly: entry.readonly,
        source: entry.host.clone(),
        target: entry.guest.clone(),
      })
      .collect();

    if self.spool().is_some() {
      mounts.push(Mount {
        readonly: false,
        source: self.host_spool(),
        target: PathBuf::from(GUEST_SPOOL_TARGET),
      });
    }

    mounts.push(Mount {
      readonly: true,
      source: self.managed_settings(),
      target: PathBuf::from(MANAGED_SETTINGS_TARGET),
    });

    mounts.push(Mount {
      readonly: false,
      source: self.claude_home(),
      target: PathBuf::from(CLAUDE_HOME_TARGET),
    });

    mounts
  }

  pub fn workdir(&self) -> PathBuf {
    self
      .workspace()
      .guest_path(&self.project_dir)
      .unwrap_or_else(|| PathBuf::from(crate::workspace::WORKSPACE_TARGET))
  }

  pub fn image(&self) -> String {
    if self.manifest.image.is_empty() {
      return self.manifest.project.image.clone();
    }

    match &self.profile {
      Some(name) => format!("compostbin/profile-{name}:latest"),
      None => format!("compostbin/{}:latest", self.container_name()),
    }
  }

  pub fn forwards(&self) -> Vec<Forward> {
    self
      .manifest
      .host
      .ports
      .iter()
      .copied()
      .map(Forward::from)
      .collect()
  }

  fn process(&self) -> Vec<String> {
    if !self.manifest.host.has_ports() {
      return KEEPALIVE_COMMAND
        .iter()
        .copied()
        .map(str::to_string)
        .collect();
    }

    std::iter::once(GUEST_PORTS_NAME.to_string())
      .chain(self.forwards().iter().map(Forward::guest_argument))
      .collect()
  }

  fn inherited_env(&self) -> Vec<EnvVar> {
    self
      .manifest
      .container
      .env
      .iter()
      .map(|name| EnvVar::Inherit(name.clone()))
      .collect()
  }

  pub(super) fn run_spec(&self) -> RunSpec {
    RunSpec {
      arguments: self.process(),
      env: self.inherited_env(),
      image: self.image(),
      mounts: self.mounts(),
      name: self.container_name(),
      resources: Resources {
        cpus: self.manifest.container.cpus,
        memory_in_bytes: self.manifest.container.memory.bytes(),
      },
      workdir: Some(self.workdir()),
    }
  }

  /// Not interactive, or it would swallow stdin meant for the attached process.
  pub(super) fn setup_spec(&self, line: &str) -> ExecSpec {
    ExecSpec {
      interactive: false,
      tty: false,
      ..self.exec_spec(["bash", "-euo", "pipefail", "-c", line].map(str::to_string))
    }
  }

  /// An exec doesn't inherit the boot environment, so `[container] env` is repeated first.
  /// `CLAUDE_CONFIG_DIR` moves `.claude.json` into the mounted home; otherwise each new container starts logged out.
  /// On Linux, Claude uses `wl-copy` only when `WAYLAND_DISPLAY` is set.
  fn exec_spec(&self, arguments: impl IntoIterator<Item = String>) -> ExecSpec {
    let mut env = self.inherited_env();
    env.extend([
      EnvVar::Set {
        name: "CLAUDE_CONFIG_DIR".to_string(),
        value: CLAUDE_HOME_TARGET.to_string(),
      },
      EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      },
    ]);

    if self.manifest.host.clipboard {
      env.push(EnvVar::Set {
        name: "WAYLAND_DISPLAY".to_string(),
        value: CLIPBOARD_DISPLAY.to_string(),
      });
    }

    ExecSpec {
      arguments: arguments.into_iter().collect(),
      env,
      interactive: true,
      name: self.container_name(),
      tty: true,
      user: None,
      workdir: Some(self.workdir()),
    }
  }

  pub(super) fn process_spec(&self, process: &Process) -> ExecSpec {
    ExecSpec {
      tty: process.tty,
      user: Some(process.user.clone()),
      ..self.exec_spec(process.argv.iter().cloned())
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::fixtures::{session, session_at};
  use std::path::Path;

  #[test]
  fn a_setup_line_reads_no_input_and_asks_for_no_terminal() {
    let spec = session().setup_spec("./bin/setup");

    assert!(!spec.interactive);
    assert!(!spec.tty);
    assert_eq!(
      spec.arguments,
      ["bash", "-euo", "pipefail", "-c", "./bin/setup"].map(str::to_string)
    );
  }

  #[test]
  fn builds_exec_spec() {
    assert_eq!(
      session().exec_spec(["claude".to_string(), "--continue".to_string()]),
      ExecSpec {
        arguments: vec!["claude".to_string(), "--continue".to_string()],
        env: vec![
          EnvVar::Inherit("ANTHROPIC_API_KEY".to_string()),
          EnvVar::Set {
            name: "CLAUDE_CONFIG_DIR".to_string(),
            value: "/home/claude/.claude".to_string(),
          },
          EnvVar::Set {
            name: "IS_SANDBOX".to_string(),
            value: "1".to_string(),
          },
        ],
        interactive: true,
        name: "compostbin-cb".to_string(),
        tty: true,
        user: None,
        workdir: Some(PathBuf::from("/workspace/workspace/compostbin")),
      }
    );
  }

  #[test]
  fn names_a_display_only_for_the_clipboard() {
    let mut session = session();
    let display = EnvVar::Set {
      name: "WAYLAND_DISPLAY".to_string(),
      value: CLIPBOARD_DISPLAY.to_string(),
    };

    assert!(!session.exec_spec([]).env.contains(&display));

    session.manifest.host.clipboard = true;

    assert!(session.exec_spec([]).env.contains(&display));
    assert!(
      session
        .mounts()
        .iter()
        .any(|mount| mount.target == Path::new(GUEST_SPOOL_TARGET)),
      "the clipboard is served through the spool"
    );
  }

  #[test]
  fn mounts_project_dir_when_outside_every_root() {
    let session = session_at(Path::new("/Users/user"), "", "code/loose");

    assert_eq!(
      session.mounts(),
      [
        Mount {
          readonly: false,
          source: "/Users/user/code/loose".into(),
          target: "/workspace/loose".into(),
        },
        Mount {
          readonly: true,
          source: "/Users/user/.local/state/compostbin/sessions/compostbin-loose/managed".into(),
          target: "/etc/claude-code".into(),
        },
        Mount {
          readonly: false,
          source: "/Users/user/.local/state/compostbin/sessions/compostbin-loose/claude-home".into(),
          target: "/home/claude/.claude".into(),
        },
      ]
    );
    assert_eq!(session.workdir(), PathBuf::from("/workspace/loose"));
  }

  #[test]
  fn skips_project_mount_when_inside_a_root() {
    let sources: Vec<String> = session()
      .mounts()
      .iter()
      .map(|mount| mount.source.display().to_string())
      .collect();

    assert_eq!(
      sources,
      [
        "/Users/user/workspace",
        "/Users/user/.cargo/registry",
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/managed",
        "/Users/user/.local/state/compostbin/sessions/compostbin-cb/claude-home",
      ]
    );
  }

  #[test]
  fn mounts_the_spool_only_when_commands_are_declared() {
    let spool = "/Users/user/.local/state/compostbin/sessions/compostbin-cb/host";

    assert!(
      !session()
        .mounts()
        .iter()
        .any(|mount| mount.source == Path::new(spool)),
      "an undeclared channel must not be mounted: {:?}",
      session().mounts()
    );

    let mut declared = session();
    declared.manifest.host =
      toml::from_str("[commands.test]\nargv = [\"cargo\", \"nextest\", \"run\"]\n").expect("host config should parse");

    assert!(
      declared.mounts().contains(&Mount {
        readonly: false,
        source: spool.into(),
        target: GUEST_SPOOL_TARGET.into(),
      }),
      "a declared command needs the spool mounted: {:?}",
      declared.mounts()
    );
  }

  #[test]
  fn builds_run_spec() {
    let sessions = "/Users/user/.local/state/compostbin/sessions/compostbin-cb";

    assert_eq!(
      session().run_spec(),
      RunSpec {
        arguments: KEEPALIVE_COMMAND.map(str::to_string).to_vec(),
        env: vec![EnvVar::Inherit("ANTHROPIC_API_KEY".to_string())],
        image: "compostbin/base:latest".to_string(),
        mounts: vec![
          Mount {
            readonly: false,
            source: PathBuf::from("/Users/user/workspace"),
            target: PathBuf::from("/workspace/workspace"),
          },
          Mount {
            readonly: true,
            source: PathBuf::from("/Users/user/.cargo/registry"),
            target: PathBuf::from("/workspace/registry"),
          },
          Mount {
            readonly: true,
            source: PathBuf::from(format!("{sessions}/managed")),
            target: PathBuf::from(MANAGED_SETTINGS_TARGET),
          },
          Mount {
            readonly: false,
            source: PathBuf::from(format!("{sessions}/claude-home")),
            target: PathBuf::from(CLAUDE_HOME_TARGET),
          },
        ],
        name: "compostbin-cb".to_string(),
        resources: Resources {
          cpus: 4,
          memory_in_bytes: 8 << 30,
        },
        workdir: Some(PathBuf::from("/workspace/workspace/compostbin")),
      }
    );
  }

  #[test]
  fn keeps_host_paths_out_of_the_container() {
    let session = session();

    let leaking: Vec<String> = session
      .mounts()
      .iter()
      .map(|mount| mount.target.display().to_string())
      .chain([session.workdir().display().to_string()])
      .filter(|guest| guest.starts_with("/Users"))
      .collect();

    assert_eq!(
      leaking,
      Vec::<String>::new(),
      "a host path may appear only as the source half of a mount"
    );
  }

  #[test]
  fn runs_the_base_image_unless_the_manifest_adds_to_it() {
    assert_eq!(session().image(), "compostbin/base:latest");

    let mut session = session();
    session.manifest.image.packages = vec!["jq".to_string()];

    assert_eq!(session.image(), "compostbin/compostbin-cb:latest");
    assert_eq!(
      session.run_spec().image,
      "compostbin/compostbin-cb:latest",
      "the session must run the image it built"
    );
  }

  #[test]
  fn runs_port_relay() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    assert_eq!(
      session.run_spec().arguments,
      [
        GUEST_PORTS_NAME.to_string(),
        Forward::from(7001).guest_argument(),
        Forward::from(7002).guest_argument(),
      ],
      "the relay is the container's own process"
    );
  }

  #[test]
  fn forwards_each_declared_port_to_the_hosts_loopback() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    assert_eq!(session.forwards(), [Forward::from(7001), Forward::from(7002)]);
  }
}
