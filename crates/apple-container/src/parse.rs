//! Turning `container` CLI output into values. One module, so drift between CLI
//! versions has a single place to be absorbed.

use serde_json::Value;
use std::net::IpAddr;

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

/// The gateway of the first network a container is attached to, from
/// `container inspect <name>`: `[0].status.networks[].ipv4Gateway`.
pub fn container_gateway(stdout: &str) -> Option<IpAddr> {
  let inspected: Value = serde_json::from_str(stdout).ok()?;

  inspected
    .get(0)?
    .get("status")?
    .get("networks")?
    .as_array()?
    .iter()
    .find_map(|network| network.get("ipv4Gateway")?.as_str()?.parse().ok())
}

/// A network's gateway, from `container network inspect <name>`:
/// `[0].status.ipv4Gateway`.
pub fn network_gateway(stdout: &str) -> Option<IpAddr> {
  let inspected: Value = serde_json::from_str(stdout).ok()?;

  inspected
    .get(0)?
    .get("status")?
    .get("ipv4Gateway")?
    .as_str()?
    .parse()
    .ok()
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

  /// `container inspect <name>`, trimmed to what is read and one field that is
  /// not, so an added field cannot break it.
  const CONTAINER_INSPECT: &str = r#"[{"status":{"networks":[{"network":"default","ipv4Address":"192.168.64.3/24","ipv4Gateway":"192.168.64.1"}],"state":"running"}}]"#;

  /// `container network inspect default`.
  const NETWORK_INSPECT: &str = r#"[{"status":{"ipv4Gateway":"192.168.64.1","ipv4Subnet":"192.168.64.0/24"}}]"#;

  #[test]
  fn reads_the_container_gateway() {
    assert_eq!(container_gateway(CONTAINER_INSPECT), "192.168.64.1".parse().ok());
  }

  #[test]
  fn reads_no_gateway_without_networks() {
    assert_eq!(container_gateway(r#"[{"status":{"networks":[]}}]"#), None);
  }

  #[test]
  fn reads_the_network_gateway() {
    assert_eq!(network_gateway(NETWORK_INSPECT), "192.168.64.1".parse().ok());
  }

  #[test]
  fn reads_no_gateway_from_unexpected_output() {
    assert_eq!(container_gateway("Error: not found"), None);
    assert_eq!(network_gateway("Error: not found"), None);
  }
}
