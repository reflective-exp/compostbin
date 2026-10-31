use crate::host::{Forward, GUEST_SOCKETS_GROUP, GUEST_SPOOL_TARGET};
use crate::image::{GUEST_PORTS_NAME, ROOT};
use crate::session::briefing::MANAGED_SETTINGS_TARGET;
use crate::session::{Process, Session};
use compostbin_engine::model::{EnvVar, ExecSpec, Mount, Resources, RunSpec};
use std::path::PathBuf;
use std::time::Duration;

pub const SOCKETS_READY_TIMEOUT: Duration = Duration::from_secs(5);
const SOCKETS_READY_POLL: Duration = Duration::from_millis(100);
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

  /// The ports, then the sockets by name.
  pub fn forwards(&self) -> Vec<Forward> {
    let host = &self.manifest.host;
    let ports = host.ports.iter().copied().map(Forward::from);
    let sockets = host
      .socket_targets()
      .zip(0..)
      .map(|((target, socket), index)| {
        Forward::socket(self.resolver.resolve(&socket.source), target, socket.mode, index)
      });

    ports.chain(sockets).collect()
  }

  fn process(&self) -> Vec<String> {
    let forwards = self.forwards();

    if forwards.is_empty() {
      return KEEPALIVE_COMMAND
        .iter()
        .copied()
        .map(str::to_string)
        .collect();
    }

    std::iter::once(GUEST_PORTS_NAME.to_string())
      .chain(forwards.iter().map(Forward::guest_argument))
      .collect()
  }

  /// `[container] env`, then `[container.environment]`, which wins over a variable both name.
  fn inherited_env(&self) -> Vec<EnvVar> {
    let container = &self.manifest.container;
    let set = container
      .environment
      .iter()
      .map(|(name, value)| EnvVar::Set {
        name: name.clone(),
        value: value.clone(),
      });

    container
      .env
      .iter()
      .filter(|name| !container.environment.contains_key(*name))
      .cloned()
      .map(EnvVar::Inherit)
      .chain(set)
      .collect()
  }

  pub(super) fn run_spec(&self) -> RunSpec {
    let container = &self.manifest.container;

    RunSpec {
      arguments: self.process(),
      env: self.inherited_env(),
      image: self.image(),
      mounts: self.mounts(),
      name: self.container_name(),
      resources: Resources {
        cpus: container.cpus,
        memory_in_bytes: container.memory.bytes(),
      },
      // Root, to make each socket at its target and give it to the sockets group. Attached processes still run
      // as the image's user.
      user: self.manifest.host.has_sockets().then(|| ROOT.to_string()),
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

  /// Exits 0 once every socket exists in the guest with its group and mode, or 1 after `SOCKETS_READY_TIMEOUT`.
  /// Both are checked because the relay may set them after making the socket. As root, since a socket's directory
  /// need not be the image user's to read. `None` without sockets.
  pub(super) fn sockets_ready_spec(&self) -> Option<ExecSpec> {
    if !self.manifest.host.has_sockets() {
      return None;
    }

    let tests: Vec<String> = self
      .manifest
      .host
      .socket_targets()
      .map(|(target, socket)| {
        let target = target.display();
        format!(
          "[ -S {target} ] && [ \"$(stat -c %G:%a {target})\" = {GUEST_SOCKETS_GROUP}:{:o} ]",
          socket.mode
        )
      })
      .collect();
    let tries = SOCKETS_READY_TIMEOUT.as_millis() / SOCKETS_READY_POLL.as_millis();
    let script = format!(
      "for _ in $(seq {tries}); do {{ {}; }} && exit 0; sleep {}; done; exit 1",
      tests.join(" && "),
      SOCKETS_READY_POLL.as_secs_f32()
    );

    Some(ExecSpec {
      user: Some(ROOT.to_string()),
      ..self.setup_spec(&script)
    })
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
  use crate::manifest::{HostSocket, RESERVED_VARIABLES, SocketMode};
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
  fn sets_the_environment_in_every_process_over_variables_passed_through() {
    let mut session = session();
    session.manifest.container.env = vec!["MODE".to_string(), "TOKEN".to_string()];
    session.manifest.container.environment = [("MODE".to_string(), "fast".to_string())].into();
    let expected = [
      EnvVar::Inherit("TOKEN".to_string()),
      EnvVar::Set {
        name: "MODE".to_string(),
        value: "fast".to_string(),
      },
    ];

    assert_eq!(session.run_spec().env, expected);
    assert_eq!(session.setup_spec("true").env[..2], expected);
    assert_eq!(session.exec_spec([]).env[..2], expected);
    assert_eq!(
      session.exec_spec([]).env[2],
      EnvVar::Set {
        name: "CLAUDE_CONFIG_DIR".to_string(),
        value: "/home/claude/.claude".to_string(),
      },
      "compostbin's own come last"
    );
  }

  #[test]
  fn reserves_every_variable_compostbin_sets() {
    let mut session = session();
    session.manifest.container.env.clear();
    session.manifest.host.clipboard = true;

    let set: Vec<String> = session
      .exec_spec([])
      .env
      .into_iter()
      .filter_map(|variable| match variable {
        EnvVar::Set { name, .. } => Some(name),
        EnvVar::Inherit(_) => None,
      })
      .collect();

    assert_eq!(set, RESERVED_VARIABLES);
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
        user: None,
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

  #[test]
  fn forwards_each_declared_socket_after_the_ports() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];
    session.manifest.host.sockets = [
      (
        "other".to_string(),
        HostSocket {
          target: Some("/run/other.sock".into()),
          mode: SocketMode::new(0o600),
          ..HostSocket::new("/host/other.sock")
        },
      ),
      ("mcp".to_string(), HostSocket::new("~/mcp.sock")),
    ]
    .into();

    assert_eq!(
      session.forwards(),
      [
        Forward::from(7001),
        Forward::socket(
          session.resolver().resolve("~/mcp.sock"),
          PathBuf::from("/run/compostbin/sockets/mcp"),
          SocketMode::default(),
          0
        ),
        Forward::socket(
          PathBuf::from("/host/other.sock"),
          PathBuf::from("/run/other.sock"),
          SocketMode::new(0o600),
          1
        ),
      ]
    );
  }

  #[test]
  fn waits_for_every_socket_as_root_without_reading_input() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];
    assert_eq!(session.sockets_ready_spec(), None, "ports are not waited for");

    session.manifest.host.sockets = [
      ("mcp".to_string(), HostSocket::new("/host/mcp.sock")),
      (
        "other".to_string(),
        HostSocket {
          target: Some("/run/other.sock".into()),
          mode: SocketMode::new(0o600),
          ..HostSocket::new("/host/other.sock")
        },
      ),
    ]
    .into();
    let spec = session.sockets_ready_spec().expect("sockets to wait for");

    assert_eq!(spec.user.as_deref(), Some("root"));
    assert!(!spec.interactive && !spec.tty);
    assert_eq!(
      spec.arguments.last().map(String::as_str),
      Some(
        "for _ in $(seq 50); do { \
         [ -S /run/compostbin/sockets/mcp ] && [ \"$(stat -c %G:%a /run/compostbin/sockets/mcp)\" = compostbin:660 ] && \
         [ -S /run/other.sock ] && [ \"$(stat -c %G:%a /run/other.sock)\" = compostbin:600 ]; \
         } && exit 0; sleep 0.1; done; exit 1"
      )
    );
  }

  #[test]
  fn runs_the_relay_as_root_for_sockets() {
    let mut session = session();
    assert_eq!(
      session.run_spec().user,
      None,
      "nothing to relay, nothing to run as root"
    );

    session.manifest.host.ports = vec![7001];
    assert_eq!(session.run_spec().user, None, "ports need no root");

    session.manifest.host.ports = Vec::new();
    session.manifest.host.sockets = [("mcp".to_string(), HostSocket::new("/host/mcp.sock"))].into();
    let spec = session.run_spec();

    assert_eq!(spec.user.as_deref(), Some("root"));
    assert_eq!(
      spec.arguments,
      [GUEST_PORTS_NAME.to_string(), session.forwards()[0].guest_argument(),]
    );
  }
}
