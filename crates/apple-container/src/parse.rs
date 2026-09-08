//! Turning `container` CLI output into values. Kept in one module so output
//! drift between CLI versions has a single place to be absorbed.

/// Names from any `--quiet` listing — images or containers — one per line.
pub fn names(stdout: &str) -> Vec<String> {
  stdout
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect()
}

/// The version from `container --version`, whose output reads
/// `container CLI version 1.3.1 (build: release, commit: unspecified)`.
pub fn cli_version(stdout: &str) -> Option<String> {
  stdout
    .split_whitespace()
    .find(|word| word.starts_with(|first: char| first.is_ascii_digit()))
    .map(str::to_string)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reads_names_one_per_line() {
    assert_eq!(
      names("debian:stable-slim\nghcr.io/apple/containerization/vminit:0.33.3\n"),
      ["debian:stable-slim", "ghcr.io/apple/containerization/vminit:0.33.3"]
    );
  }

  #[test]
  fn reads_no_names_from_empty_output() {
    assert_eq!(names("\n  \n"), Vec::<String>::new());
  }

  #[test]
  fn reads_the_cli_version() {
    assert_eq!(
      cli_version("container CLI version 1.3.1 (build: release, commit: unspecified)"),
      Some("1.3.1".to_string())
    );
  }

  #[test]
  fn reads_no_version_from_unexpected_output() {
    assert_eq!(cli_version("command not found"), None);
  }
}
