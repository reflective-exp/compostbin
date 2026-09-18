//! Turning `compostbin_engine`'s specs into what the bridge carries.
//!
//! Lists cross as newline-separated strings, mounts as tab-separated triples
//! within them. Neither separator can occur in the values: a mount is two
//! host paths and a flag, and `host::request::Request` refuses arguments
//! containing a newline.

use compostbin_engine::model::{EnvVar, Mount, SocketRelay};
use std::path::Path;

/// One element per line. Swift reads `""` as no elements, not one empty one.
pub fn lines(values: &[String]) -> String {
  values.join("\n")
}

/// The guest's working directory; `/` when none is declared.
pub fn working_directory(workdir: Option<&Path>) -> String {
  workdir.map_or_else(|| "/".to_string(), |path| path.display().to_string())
}

/// `source\tdestination\tro|rw`, in declared order (matters for nested mounts).
pub fn mounts(mounts: &[Mount]) -> Vec<String> {
  mounts
    .iter()
    .map(|mount| {
      format!(
        "{}\t{}\t{}",
        mount.source.display(),
        mount.target.display(),
        if mount.readonly { "ro" } else { "rw" }
      )
    })
    .collect()
}

/// `source\tdestination`.
///
/// Not mounts: a socket mounted as a filesystem relays nothing, so
/// Containerization configures relays separately.
pub fn sockets(sockets: &[SocketRelay]) -> Vec<String> {
  sockets
    .iter()
    .map(|socket| format!("{}\t{}", socket.source.display(), socket.target.display()))
    .collect()
}

/// `NAME=VALUE`.
///
/// An `Inherit` unset on the host is dropped, not passed empty, so the guest
/// can tell unset from empty.
pub fn environment(env: &[EnvVar]) -> Vec<String> {
  env
    .iter()
    .filter_map(|variable| match variable {
      EnvVar::Inherit(name) => std::env::var(name)
        .ok()
        .map(|value| format!("{name}={value}")),
      EnvVar::Set { name, value } => Some(format!("{name}={value}")),
    })
    .collect()
}

/// Gateway of Virtualization.framework's built-in NAT (macOS shared networking).
///
/// Not vmnet: that needs privileges, hence a privileged helper. NAT needs only
/// the virtualization entitlement.
pub const NAT_GATEWAY: &str = "192.168.64.1";
const NAT_PREFIX: u32 = 24;
/// `.1` is the gateway and `.255` the broadcast; guests live between these.
const FIRST_HOST: u32 = 2;
const LAST_HOST: u32 = 250;

/// The address a session's guest takes on the NAT network.
///
/// Static because `Interface` requires an address up front and vminitd
/// configures it without DHCP. Hashed from the session name: stable per
/// session, distinct between concurrent sessions.
///
/// Collisions with other hosts on the shared network go undetected.
pub fn nat_address(name: &str) -> String {
  // FNV-1a: short and well spread.
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

  for byte in name.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }

  let host = FIRST_HOST + (hash % u64::from(LAST_HOST - FIRST_HOST + 1)) as u32;
  let (network, _) = NAT_GATEWAY
    .rsplit_once('.')
    .expect("the gateway is a dotted quad");

  format!("{network}.{host}/{NAT_PREFIX}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::PathBuf;

  #[test]
  fn writes_mounts_in_declared_order_with_their_mode() {
    let declared = [
      Mount {
        readonly: false,
        source: PathBuf::from("/Users/user/workspace"),
        target: PathBuf::from("/workspace"),
      },
      Mount {
        readonly: true,
        source: PathBuf::from("/Users/user/.cargo/registry"),
        target: PathBuf::from("/Users/user/.cargo/registry"),
      },
    ];

    assert_eq!(
      lines(&mounts(&declared)),
      "/Users/user/workspace\t/workspace\trw\n/Users/user/.cargo/registry\t/Users/user/.cargo/registry\tro"
    );
  }

  #[test]
  fn writes_no_mounts_as_nothing_at_all() {
    assert_eq!(lines(&mounts(&[])), "");
    assert_eq!(lines(&[]), "");
    assert_eq!(lines(&sockets(&[])), "");
  }

  #[test]
  fn works_in_the_root_unless_told_otherwise() {
    assert_eq!(working_directory(None), "/");
    assert_eq!(working_directory(Some(Path::new("/workspace"))), "/workspace");
  }

  #[test]
  fn writes_a_relayed_socket_as_a_pair_of_paths() {
    let declared = [SocketRelay {
      source: PathBuf::from("/state/ports/7001.sock"),
      target: PathBuf::from("/run/compostbin/ports/7001.sock"),
    }];

    assert_eq!(
      sockets(&declared),
      ["/state/ports/7001.sock\t/run/compostbin/ports/7001.sock"]
    );
  }

  #[test]
  fn sets_a_variable_and_drops_an_unset_inherited_one() {
    // SAFETY: single-threaded test, and the name is this test's own.
    unsafe { std::env::set_var("COMPOSTBIN_SPEC_TEST", "present") };

    let declared = [
      EnvVar::Inherit("COMPOSTBIN_SPEC_TEST".to_string()),
      EnvVar::Inherit("COMPOSTBIN_SPEC_TEST_UNSET".to_string()),
      EnvVar::Set {
        name: "IS_SANDBOX".to_string(),
        value: "1".to_string(),
      },
    ];

    assert_eq!(environment(&declared), ["COMPOSTBIN_SPEC_TEST=present", "IS_SANDBOX=1"]);
  }

  #[test]
  fn gives_a_session_a_stable_address_on_the_nat_network() {
    let address = nat_address("compostbin-compostbin");

    assert_eq!(nat_address("compostbin-compostbin"), address);
    assert_ne!(nat_address("compostbin-mudbrick"), address);
  }

  #[test]
  fn keeps_every_address_inside_the_gateways_subnet() {
    for name in ["a", "compostbin-compostbin", "compostbin-mudbrick", "", "-"] {
      let address = nat_address(name);
      let host: u32 = address
        .strip_prefix("192.168.64.")
        .and_then(|rest| rest.strip_suffix("/24"))
        .unwrap_or_else(|| panic!("{name} -> {address} should sit in the gateway's /24"))
        .parse()
        .expect("a host octet");

      assert!(
        (FIRST_HOST..=LAST_HOST).contains(&host),
        "{name} -> {address} should avoid the gateway and the broadcast address"
      );
    }
  }
}
