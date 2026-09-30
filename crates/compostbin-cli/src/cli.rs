use clap::{Args, Parser, Subcommand};
use compostbin_core::session::image;

#[derive(Debug, Parser)]
#[command(name = "compostbin", about = "Run Claude Code in a container")]
#[clap(version)]
pub struct Arguments {
  #[command(subcommand)]
  pub command: Command,
}

#[derive(Debug, PartialEq, Subcommand)]
pub enum Command {
  /// Mount a path into the session
  Add {
    /// The host path to mount, `~` included
    path: String,
    /// Mount a path `add` would otherwise refuse as too broad or too sensitive
    #[arg(long)]
    force: bool,
    /// Record it in the uncommitted `.config/compostbin.local.toml` instead of
    /// the manifest the project shares
    #[arg(long)]
    local: bool,
    #[arg(long)]
    readonly: bool,
  },
  /// Build the base image, and the project's own image if the manifest adds to it
  Build {
    /// Skip cached rootfs snapshots
    #[arg(long)]
    no_cache: bool,
    #[command(flatten)]
    config: Config,
  },
  /// Remove this session's transient state
  Clean {
    /// Also remove Claude's home, discarding the conversation `--continue` resumes
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    config: Config,
  },
  /// Check the host, the image store, and every declared path
  Doctor {
    #[command(flatten)]
    config: Config,
  },
  /// Write a manifest with defaults
  Init,
  /// Install or update compostbin's Claude skills in ~/.claude/skills
  Install {
    /// Apply the changes without asking
    #[arg(short, long)]
    yes: bool,
  },
  /// List the session's mounts
  Ls {
    #[command(flatten)]
    config: Config,
  },
  /// Start or join the session and run a command in it
  Exec {
    /// The command and its arguments
    #[arg(required = true, trailing_var_arg = true)]
    argv: Vec<String>,
    #[command(flatten)]
    config: Config,
    /// Give it a terminal, which needs one on this side too
    #[arg(short = 't', long)]
    tty: bool,
    /// The guest user to run it as
    #[arg(short = 'U', long, default_value = image::USER)]
    user: String,
  },
  /// Start or join the session and attach Claude
  Run {
    /// Arguments passed to Claude
    #[arg(last = true)]
    arguments: Vec<String>,
    #[command(flatten)]
    config: Config,
  },
  /// Open a shell in the session: `exec -t bash`
  Shell {
    #[command(flatten)]
    config: Config,
    /// The guest user to open it as
    #[arg(short = 'U', long, default_value = image::USER)]
    user: String,
  },
}

/// Where a session command reads its configuration.
#[derive(Args, Debug, Default, PartialEq)]
pub struct Config {
  /// Read ~/.config/compostbin/profiles/<PROFILE>.toml instead of the
  /// project's manifest
  #[arg(long, value_parser = profile_name)]
  pub profile: Option<String>,
}

/// A file name under the profiles directory, not a path out of it.
fn profile_name(name: &str) -> Result<String, String> {
  if name.is_empty() || name.starts_with('.') || name.contains('/') {
    return Err(format!("\"{name}\" is not a profile name"));
  }

  Ok(name.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn parse(argv: &[&str]) -> Command {
    Arguments::parse_from(argv).command
  }

  #[test]
  fn parses_add() {
    assert_eq!(
      parse(&["compostbin", "add", "../libfoo", "--readonly"]),
      Command::Add {
        force: false,
        local: false,
        path: "../libfoo".to_string(),
        readonly: true,
      }
    );
    assert_eq!(
      parse(&["compostbin", "add", "../libfoo"]),
      Command::Add {
        force: false,
        local: false,
        path: "../libfoo".to_string(),
        readonly: false,
      }
    );
  }

  #[test]
  fn parses_run_with_trailing_claude_arguments() {
    let claude = |arguments: &[&str]| Command::Run {
      arguments: arguments
        .iter()
        .map(|argument| argument.to_string())
        .collect(),
      config: Config::default(),
    };

    assert_eq!(
      parse(&["compostbin", "run", "--", "--continue", "--model", "opus"]),
      claude(&["--continue", "--model", "opus"])
    );
    assert_eq!(parse(&["compostbin", "run"]), claude(&[]));
  }

  /// The command's own flags are its own: only what precedes it is ours.
  #[test]
  fn parses_exec_with_the_commands_own_arguments() {
    assert_eq!(
      parse(&["compostbin", "exec", "-U", "root", "apt-cache", "search", "ripgrep"]),
      Command::Exec {
        argv: ["apt-cache", "search", "ripgrep"]
          .map(str::to_string)
          .to_vec(),
        config: Config::default(),
        tty: false,
        user: "root".to_string(),
      }
    );
    assert_eq!(
      parse(&["compostbin", "exec", "ls", "-l"]),
      Command::Exec {
        argv: ["ls", "-l"].map(str::to_string).to_vec(),
        config: Config::default(),
        tty: false,
        user: "claude".to_string(),
      }
    );
  }

  #[test]
  fn parses_exec_asking_for_a_terminal() {
    assert_eq!(
      parse(&["compostbin", "exec", "-t", "--user", "root", "bash"]),
      Command::Exec {
        argv: vec!["bash".to_string()],
        config: Config::default(),
        tty: true,
        user: "root".to_string(),
      }
    );
  }

  #[test]
  fn refuses_an_exec_with_no_command() {
    assert!(Arguments::try_parse_from(["compostbin", "exec"]).is_err());
  }

  #[test]
  fn parses_bare_subcommands() {
    let clean = |all| Command::Clean {
      all,
      config: Config::default(),
    };

    assert_eq!(parse(&["compostbin", "clean"]), clean(false));
    assert_eq!(parse(&["compostbin", "clean", "--all"]), clean(true));
    assert_eq!(parse(&["compostbin", "init"]), Command::Init);
    assert_eq!(
      parse(&["compostbin", "ls"]),
      Command::Ls {
        config: Config::default()
      }
    );
  }

  #[test]
  fn parses_run_with_a_profile() {
    assert_eq!(
      parse(&["compostbin", "run", "--profile", "rust", "--", "--continue"]),
      Command::Run {
        arguments: vec!["--continue".to_string()],
        config: Config {
          profile: Some("rust".to_string()),
        },
      }
    );
  }

  /// They write the project's manifest; profiles are written by hand.
  #[test]
  fn refuses_a_profile_on_add_and_init() {
    assert!(Arguments::try_parse_from(["compostbin", "add", "../libfoo", "--profile", "rust"]).is_err());
    assert!(Arguments::try_parse_from(["compostbin", "init", "--profile", "rust"]).is_err());
  }

  #[test]
  fn refuses_a_path_as_a_profile_name() {
    for name in ["../escape", "a/b", ".hidden", ""] {
      assert!(
        Arguments::try_parse_from(["compostbin", "run", "--profile", name]).is_err(),
        "{name:?}"
      );
    }
  }

  #[test]
  fn parses_shell_as_claude_unless_told_otherwise() {
    let shell = |user: &str| Command::Shell {
      config: Config::default(),
      user: user.to_string(),
    };

    assert_eq!(parse(&["compostbin", "shell"]), shell("claude"));
    assert_eq!(parse(&["compostbin", "shell", "-U", "root"]), shell("root"));
    assert_eq!(parse(&["compostbin", "shell", "--user", "root"]), shell("root"));
  }
}
