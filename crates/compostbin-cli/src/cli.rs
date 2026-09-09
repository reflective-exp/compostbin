use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
  name = "compostbin",
  about = "Run Claude Code in a container with the host filesystem live"
)]
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
    #[arg(long)]
    readonly: bool,
    /// Recreate the container so an out-of-root path becomes visible
    #[arg(long)]
    restart: bool,
  },
  /// Build the base image, and the project's own image if the manifest adds to it
  Build,
  /// Remove this session's transient state
  Clean {
    /// Also remove Claude's home, discarding the conversation `--continue` resumes
    #[arg(long)]
    all: bool,
  },
  /// Check the host, the daemon, and every declared path
  Doctor,
  /// Serve host commands for a session started elsewhere
  HostAgent,
  /// Write a manifest with detected defaults
  Init,
  /// List the session's mounts
  Ls,
  /// Start the session and attach Claude
  Run {
    /// Arguments passed through to `claude`
    #[arg(last = true)]
    arguments: Vec<String>,
  },
  /// Open a shell in the session
  Shell,
  /// Stop and delete the session container
  Stop,
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
      parse(&["compostbin", "add", "../libfoo", "--readonly", "--restart"]),
      Command::Add {
        force: false,
        path: PathBuf::from("../libfoo"),
        readonly: true,
        restart: true,
      }
    );
    assert_eq!(
      parse(&["compostbin", "add", "../libfoo"]),
      Command::Add {
        force: false,
        path: PathBuf::from("../libfoo"),
        readonly: false,
        restart: false,
      }
    );
  }

  #[test]
  fn parses_run_with_trailing_claude_arguments() {
    assert_eq!(
      parse(&["compostbin", "run", "--", "--continue", "--model", "opus"]),
      Command::Run {
        arguments: vec!["--continue".to_string(), "--model".to_string(), "opus".to_string()],
      }
    );
    assert_eq!(parse(&["compostbin", "run"]), Command::Run { arguments: Vec::new() });
  }

  #[test]
  fn parses_bare_subcommands() {
    assert_eq!(parse(&["compostbin", "clean"]), Command::Clean { all: false });
    assert_eq!(parse(&["compostbin", "clean", "--all"]), Command::Clean { all: true });
    assert_eq!(parse(&["compostbin", "host-agent"]), Command::HostAgent);
    assert_eq!(parse(&["compostbin", "init"]), Command::Init);
    assert_eq!(parse(&["compostbin", "ls"]), Command::Ls);
    assert_eq!(parse(&["compostbin", "shell"]), Command::Shell);
    assert_eq!(parse(&["compostbin", "stop"]), Command::Stop);
  }
}
