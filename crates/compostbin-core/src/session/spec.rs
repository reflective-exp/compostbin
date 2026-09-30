//! What a session asks the engine for: the container it creates, and each
//! process it attaches to it.

use crate::host::{Forward, GUEST_PORTS_TARGET, GUEST_SPOOL_TARGET};
use crate::session::briefing::MANAGED_SETTINGS_TARGET;
use crate::session::image::GUEST_PORTS_NAME;
use crate::session::{Process, Session};
use compostbin_engine::model::{EnvVar, ExecSpec, Mount, Resources, RunSpec, SocketRelay};
use std::path::PathBuf;

/// Where Claude's home is mounted inside the container, which runs as `claude`.
pub const CLAUDE_HOME_TARGET: &str = "/home/claude/.claude";
/// The container's own process, keeping it alive so `exec` has something to
/// attach to.
pub const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "infinity"];
/// Names no real compositor: nothing in the guest draws, it only has to be set.
pub const CLIPBOARD_DISPLAY: &str = "compostbin-clipboard";

impl Session {
  /// Every workspace entry, then the spool, then Claude's home. Order is
  /// preserved so a nested mount declared later lands on top of its parent.
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

    // Unconditional, unlike the spool: a session with no host commands still
    // needs to know it is in a container. Read-only, because the guest changing
    // what it is told is the whole point of managed settings.
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

  /// One socket per declared port, relayed into the guest rather than mounted,
  /// and named after the port so the guest's relay finds it unprompted.
  ///
  /// Each must be a live socket when the container is created, which is why
  /// `run` binds them first.
  pub fn sockets(&self) -> Vec<SocketRelay> {
    self
      .forwards()
      .into_iter()
      .map(|forward| SocketRelay {
        target: PathBuf::from(GUEST_PORTS_TARGET).join(
          forward
            .listen
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default(),
        ),
        source: forward.listen,
      })
      .collect()
  }

  /// Where the session lands inside the container. The `/workspace` fallback
  /// keeps a misconfigured manifest from starting in an unmounted directory.
  pub fn workdir(&self) -> PathBuf {
    self
      .workspace()
      .guest_path(&self.project_dir)
      .unwrap_or_else(|| PathBuf::from(crate::workspace::WORKSPACE_TARGET))
  }

  /// A derived image when the manifest adds packages or build steps, otherwise
  /// the shared base itself. A profile's is named after the profile: it is the
  /// same image in every directory.
  pub fn image(&self) -> String {
    if self.manifest.image.is_empty() {
      return self.manifest.project.image.clone();
    }

    match &self.profile {
      Some(name) => format!("compostbin/profile-{name}:latest"),
      None => format!("compostbin/{}:latest", self.container_name()),
    }
  }

  /// Each declared port, as a socket in this session's directory relayed to the
  /// host's loopback.
  pub fn forwards(&self) -> Vec<Forward> {
    self
      .manifest
      .host
      .ports
      .iter()
      .map(|&port| Forward::to_loopback(&self.port_sockets(), port))
      .collect()
  }

  /// The container's own process: the port relay when ports are declared —
  /// which fixes them at creation, like the mounts — and otherwise only
  /// something to keep it alive for `exec`.
  fn process(&self) -> Vec<String> {
    if !self.manifest.host.has_ports() {
      return KEEPALIVE_COMMAND
        .iter()
        .copied()
        .map(str::to_string)
        .collect();
    }

    std::iter::once(GUEST_PORTS_NAME.to_string())
      .chain(self.manifest.host.ports.iter().map(u16::to_string))
      .collect()
  }

  /// `[container] env`, passed through from the host.
  fn inherited_env(&self) -> Vec<EnvVar> {
    self
      .manifest
      .container
      .env
      .iter()
      .map(|name| EnvVar::Inherit(name.clone()))
      .collect()
  }

  pub fn run_spec(&self) -> RunSpec {
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
      sockets: self.sockets(),
      workdir: Some(self.workdir()),
    }
  }

  /// One `[container] setup` line, as the process that runs it.
  ///
  /// Neither interactive nor on a terminal: a setup line is the session's own
  /// work, not the caller's. Given the caller's stdin it would read what was
  /// meant for the process being attached, swallowing a piped prompt before
  /// Claude starts.
  pub fn setup_spec(&self, line: &str) -> ExecSpec {
    ExecSpec {
      interactive: false,
      tty: false,
      ..self.exec_spec(&["bash", "-euo", "pipefail", "-c", line].map(str::to_string))
    }
  }

  /// An exec doesn't inherit the boot environment, so `[container] env` is
  /// repeated here, first, so the variables below override it.
  ///
  /// `IS_SANDBOX=1` tells Claude it is already sandboxed.
  ///
  /// `CLAUDE_CONFIG_DIR` moves `.claude.json` — the account, the onboarding
  /// answers, the per-project trust — into the bind-mounted home. It defaults to
  /// `~/.claude.json`, outside the mount, so without this every new container
  /// starts logged out however faithfully `~/.claude` is preserved.
  ///
  /// `WAYLAND_DISPLAY`, with `[host] clipboard`, is what makes Claude copy at
  /// all: on Linux it looks for `wl-copy` only when there is a display to copy
  /// to. There is none, but the guest's `wl-copy` sends to the host's.
  pub fn exec_spec(&self, arguments: &[String]) -> ExecSpec {
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
      arguments: arguments.to_vec(),
      env,
      interactive: true,
      name: self.container_name(),
      tty: true,
      user: None,
      workdir: Some(self.workdir()),
    }
  }

  /// `exec_spec` for an attached process, as its user and on its streams.
  pub fn process_spec(&self, process: &Process) -> ExecSpec {
    ExecSpec {
      tty: process.tty,
      user: Some(process.user.clone()),
      ..self.exec_spec(&process.argv)
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::fixtures::session;
  use crate::workspace::paths::PathResolver;

  /// Otherwise a line reads the stdin a piped `run -- -p` meant for Claude.
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
      session().exec_spec(&["claude".to_string(), "--continue".to_string()]),
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

  /// Claude copies through `wl-copy` only when a display says there is somewhere
  /// to copy to.
  #[test]
  fn names_a_display_only_for_the_clipboard() {
    let mut session = session();
    let display = EnvVar::Set {
      name: "WAYLAND_DISPLAY".to_string(),
      value: CLIPBOARD_DISPLAY.to_string(),
    };

    assert!(!session.exec_spec(&[]).env.contains(&display));

    session.manifest.host.clipboard = true;

    assert!(session.exec_spec(&[]).env.contains(&display));
    assert!(
      session
        .mounts()
        .iter()
        .any(|mount| mount.target == PathBuf::from(GUEST_SPOOL_TARGET)),
      "the clipboard is served through the spool"
    );
  }

  #[test]
  fn mounts_project_dir_when_outside_every_root() {
    let session = Session::new(
      toml::from_str("").expect("empty manifest should parse"),
      PathResolver::new("/Users/user/code/loose", "/Users/user"),
      "/Users/user/code/loose",
    );

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

  /// No allowlist means no channel at all: the spool must be absent from the
  /// argv, not merely unused.
  #[test]
  fn mounts_the_spool_only_when_commands_are_declared() {
    let spool = "/Users/user/.local/state/compostbin/sessions/compostbin-cb/host";

    assert!(
      !session()
        .mounts()
        .iter()
        .any(|mount| mount.source == PathBuf::from(spool)),
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
        sockets: Vec::new(),
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

  /// The relay is the container's process, so its ports are fixed at creation
  /// like the mounts; with none, the container only has to stay alive.
  #[test]
  fn runs_port_relay() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    assert_eq!(
      session.run_spec().arguments,
      [GUEST_PORTS_NAME, "7001", "7002"],
      "the relay is the container's own process"
    );
  }

  #[test]
  fn forwards_through_a_socket_in_the_session_directory() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];

    assert_eq!(
      session.forwards(),
      [Forward::to_loopback(&session.port_sockets(), 7001)]
    );
    assert_eq!(
      session.forwards()[0].listen,
      session.state_dir().join("ports").join("7001.sock")
    );
  }

  /// Relayed rather than mounted, and named after the port, since the guest's
  /// relay finds it by name.
  #[test]
  fn relays_a_socket_per_declared_port() {
    let mut session = session();
    session.manifest.host.ports = vec![7001, 7002];

    let sockets = session.sockets();

    for port in [7001, 7002] {
      let source = session
        .state_dir()
        .join("ports")
        .join(format!("{port}.sock"));
      let socket = sockets
        .iter()
        .find(|socket| socket.source == source)
        .unwrap_or_else(|| panic!("port {port} should be relayed: {sockets:?}"));

      assert_eq!(
        socket.target,
        PathBuf::from(format!("/run/compostbin/ports/{port}.sock"))
      );
    }
  }

  /// A socket mounted as a filesystem is not a relay, and the framework engine
  /// would attempt exactly that mount.
  #[test]
  fn mounts_no_socket_for_a_declared_port() {
    let mut session = session();
    session.manifest.host.ports = vec![7001];

    assert!(
      !session
        .mounts()
        .iter()
        .any(|mount| mount.source.starts_with(session.port_sockets())),
      "a declared port is relayed, never mounted"
    );
  }

  #[test]
  fn relays_no_sockets_without_ports() {
    let session = session();

    assert!(
      session.sockets().is_empty(),
      "a session declaring no ports relays nothing"
    );
  }
}
