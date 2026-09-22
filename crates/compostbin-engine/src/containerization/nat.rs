//! Where compostbin puts a guest on the NAT network.
//!
//! Virtualization.framework's built-in NAT hands out no leases — the guest
//! agent sets its address directly — so something has to allocate, and that is
//! compostbin's choice rather than the framework's.

use containerization_framework::Network;

/// Gateway of Virtualization.framework's built-in NAT (macOS shared networking).
///
/// Not vmnet: that needs privileges, hence a privileged helper. NAT needs only
/// the virtualization entitlement.
pub const GATEWAY: &str = "192.168.64.1";
const PREFIX: u32 = 24;
/// `.1` is the gateway and `.255` the broadcast; guests live between these.
const FIRST_HOST: u32 = 2;
const LAST_HOST: u32 = 250;

/// The network a session's guest joins.
///
/// Static because the interface requires an address up front. Hashed from the
/// session name: stable per session, distinct between concurrent sessions.
///
/// Collisions with other hosts on the shared network go undetected.
pub fn network(name: &str) -> Network {
  Network {
    ipv4_address: address(name),
    ipv4_gateway: GATEWAY.to_string(),
  }
}

fn address(name: &str) -> String {
  // FNV-1a: short and well spread.
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

  for byte in name.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }

  let host = FIRST_HOST + (hash % u64::from(LAST_HOST - FIRST_HOST + 1)) as u32;
  let (network, _) = GATEWAY
    .rsplit_once('.')
    .expect("the gateway is a dotted quad");

  format!("{network}.{host}/{PREFIX}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn gives_a_session_a_stable_address_on_the_nat_network() {
    let first = network("session-one");

    assert_eq!(network("session-one"), first);
    assert_ne!(network("session-two"), first);
    assert_eq!(first.ipv4_gateway, GATEWAY);
  }

  #[test]
  fn keeps_every_address_inside_the_gateways_subnet() {
    for name in ["a", "session-one", "session-two", "", "-"] {
      let address = network(name).ipv4_address;
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
