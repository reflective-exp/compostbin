use std::net::Ipv4Addr;

const LEASES: &str = "/var/db/dhcpd_leases";

fn octets(text: &str) -> Option<[u8; 6]> {
  let mut octets = [0u8; 6];
  let mut parts = text.split(':');

  for octet in &mut octets {
    *octet = u8::from_str_radix(parts.next()?, 16).ok()?;
  }

  parts.next().is_none().then_some(octets)
}

/// The address the Mac's DHCP server (Virtualization's NAT) leased to `mac`, if any.
pub fn lease(mac: [u8; 6]) -> Option<Ipv4Addr> {
  leased(&std::fs::read_to_string(LEASES).ok()?, mac)
}

/// bootpd writes one `{ … }` block per lease, its octets without leading zeros:
/// `hw_address=1,a6:1:2b:…`.
fn leased(leases: &str, mac: [u8; 6]) -> Option<Ipv4Addr> {
  leases.split('{').find_map(|block| {
    let field = |name: &str| {
      block
        .lines()
        .find_map(|line| line.trim().strip_prefix(name)?.strip_prefix('='))
    };
    let hardware = field("hw_address")?.split_once(',')?.1;

    (octets(hardware)? == mac)
      .then(|| field("ip_address")?.parse().ok())
      .flatten()
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  const LEASES: &str = "{\n\tname=other\n\tip_address=192.168.64.2\n\thw_address=1,de:ad:be:ef:0:1\n}\n{\n\tname=spike\n\tip_address=192.168.64.7\n\thw_address=1,a6:1:2b:c:0:ff\n\tlease=0x6729b0c1\n}\n";

  #[test]
  fn finds_the_address_leased_to_a_mac_written_without_leading_zeros() {
    assert_eq!(
      leased(LEASES, [0xa6, 0x01, 0x2b, 0x0c, 0x00, 0xff]),
      Some(Ipv4Addr::new(192, 168, 64, 7))
    );
  }

  #[test]
  fn finds_nothing_for_a_mac_without_a_lease() {
    assert_eq!(leased(LEASES, [0xa6, 0x01, 0x2b, 0x0c, 0x00, 0xfe]), None);
  }
}
