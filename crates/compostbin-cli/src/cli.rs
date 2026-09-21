use clap::{Parser, Subcommand};
use compostbin_core::session::image;
use std::path::PathBuf;

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
    path: PathBuf,
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
  },
  /// Remove this session's transient state
  Clean {
    /// Also remove Claude's home, discarding the conversation `--continue` resumes
    #[arg(long)]
    all: bool,
  },
  /// Check the host, the image store, and every declared path
  Doctor,
  /// Write a manifest with defaults
  Init,
  /// List the session's mounts
  Ls,
  /// Start or join the session and run a command in it
  Exec {
    /// The command and its arguments
    #[arg(required = true, trailing_var_arg = true)]
    argv: Vec<String>,
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
  },
  /// Open a shell in the session: `exec -t bash`
  Shell {
    /// The guest user to open it as
    #[arg(short = 'U', long, default_value = image::USER)]
    user: String,
  },
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
        path: PathBuf::from("../libfoo"),
        readonly: true,
      }
    );
    assert_eq!(
      parse(&["compostbin", "add", "../libfoo"]),
      Command::Add {
        force: false,
        local: false,
        path: PathBuf::from("../libfoo"),
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
        tty: false,
        user: "root".to_string(),
      }
    );
    assert_eq!(
      parse(&["compostbin", "exec", "ls", "-l"]),
      Command::Exec {
        argv: ["ls", "-l"].map(str::to_string).to_vec(),
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
    assert_eq!(parse(&["compostbin", "clean"]), Command::Clean { all: false });
    assert_eq!(parse(&["compostbin", "clean", "--all"]), Command::Clean { all: true });
    assert_eq!(parse(&["compostbin", "init"]), Command::Init);
    assert_eq!(parse(&["compostbin", "ls"]), Command::Ls);
  }

  #[test]
  fn parses_shell_as_claude_unless_told_otherwise() {
    let shell = |user: &str| Command::Shell { user: user.to_string() };

    assert_eq!(parse(&["compostbin", "shell"]), shell("claude"));
    assert_eq!(parse(&["compostbin", "shell", "-U", "root"]), shell("root"));
    assert_eq!(parse(&["compostbin", "shell", "--user", "root"]), shell("root"));
  }
}
