//! Virtualization.framework's NAT hands out no leases, so guest addresses are allocated here.

use containerization_framework::containerization::network::{DNS, Interface, NATInterface};
use containerization_framework::containerization_extras::address::{CIDRv4, IPv4Address};

/// Virtualization.framework's built-in NAT, which unlike vmnet needs no privileges.
pub const GATEWAY: &str = "192.168.64.1";
const PREFIX: u32 = 24;
const FIRST_HOST: u32 = 2;
const LAST_HOST: u32 = 250;

/// The address is hashed from `name`; collisions with other hosts on the network go undetected.
pub fn interface(name: &str) -> Result<Interface, containerization_framework::Error> {
  Ok(Interface::Nat(NATInterface::new(
    CIDRv4::parse(&address(name))?,
    Some(IPv4Address::parse(GATEWAY)?),
  )))
}

pub fn dns() -> DNS {
  DNS {
    nameservers: vec![GATEWAY.to_string()],
    ..DNS::default()
  }
}

fn address(name: &str) -> String {
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
    let first = interface("session-one").unwrap();

    assert_eq!(interface("session-one").unwrap(), first);
    assert_ne!(interface("session-two").unwrap(), first);
    assert_eq!(first.ipv4_gateway(), Some(IPv4Address::parse(GATEWAY).unwrap()));
  }

  #[test]
  fn keeps_every_address_inside_the_gateways_subnet() {
    for name in ["a", "session-one", "session-two", "", "-"] {
      let address = address(name);
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
