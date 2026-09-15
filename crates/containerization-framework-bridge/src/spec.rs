//! Turning `apple_container`'s specs into what the bridge carries.
//!
//! Lists cross as newline-separated strings, and mounts as tab-separated
//! triples inside them. Neither separator can occur in what compostbin puts
//! there: a mount is two host paths and a flag, and `host::request::Request`
//! already refuses an argument containing a newline on the other side of the
//! container.

use apple_container::model::{EnvVar, Mount};

/// One list element per line. Empty in, empty out — Swift reads `""` as no
/// elements rather than as one empty one.
pub fn lines(values: &[String]) -> String {
  values.join("\n")
}

/// `source\tdestination\tro?`, in the order the spec declares them, which
/// matters when one mounted path nests inside another.
pub fn mounts(mounts: &[Mount]) -> String {
  let lines: Vec<String> = mounts
    .iter()
    .map(|mount| {
      format!(
        "{}\t{}\t{}",
        mount.source.display(),
        mount.target.display(),
        if mount.readonly { "ro" } else { "rw" }
      )
    })
    .collect();

  lines.join("\n")
}

/// `NAME=VALUE` per line.
///
/// An `Inherit` whose variable is not set on the host is dropped rather than
/// passed empty: the guest telling an unset variable from an empty one is worth
/// more than the reminder that the manifest asked for it.
pub fn environment(env: &[EnvVar]) -> String {
  let lines: Vec<String> = env
    .iter()
    .filter_map(|variable| match variable {
      EnvVar::Inherit(name) => std::env::var(name)
        .ok()
        .map(|value| format!("{name}={value}")),
      EnvVar::Set { name, value } => Some(format!("{name}={value}")),
    })
    .collect();

  lines.join("\n")
}

/// The gateway of the network Virtualization.framework's built-in NAT puts a
/// guest on — macOS's shared networking, the same one every other VZ VM uses.
///
/// Not vmnet: creating a vmnet network wants privileges an unentitled binary
/// does not have, which is why the `container` CLI runs its vmnet plugin as a
/// separate helper. NAT needs nothing beyond the virtualization entitlement.
pub const NAT_GATEWAY: &str = "192.168.64.1";
/// The prefix length of that network.
const NAT_PREFIX: u32 = 24;
/// `.1` is the gateway and `.255` the broadcast, so guests live between these.
const FIRST_HOST: u32 = 2;
const LAST_HOST: u32 = 250;

/// The address a session's guest takes on the NAT network.
///
/// Static because nothing assigns one: `Interface` requires an address up
/// front, and vminitd configures it directly rather than asking for a lease.
/// Derived from the session name so that it is stable across runs of the same
/// session and different between two sessions — which is the collision that
/// would actually happen, two projects open at once.
///
/// It can still collide with something else on the host's shared network, and
/// there is nothing here that would notice.
pub fn nat_address(name: &str) -> String {
  // FNV-1a, for no reason beyond being short and well spread.
  let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

  for byte in name.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
  }

  let host = FIRST_HOST + (hash % u64::from(LAST_HOST - FIRST_HOST + 1)) as u32;
  let gateway: Vec<&str> = NAT_GATEWAY.rsplitn(2, '.').collect();

  format!("{}.{host}/{NAT_PREFIX}", gateway[1])
}

/// `8G`, `512M`, `1024` — the forms the manifest's `[container] memory` takes,
/// in bytes. An unparseable value is `None`, and the caller defaults.
pub fn memory(memory: &str) -> Option<u64> {
  let memory = memory.trim();
  let (digits, scale) = match memory.chars().last()? {
    'g' | 'G' => (&memory[..memory.len() - 1], 1024 * 1024 * 1024),
    'm' | 'M' => (&memory[..memory.len() - 1], 1024 * 1024),
    'k' | 'K' => (&memory[..memory.len() - 1], 1024),
    _ => (memory, 1),
  };

  digits.trim().parse::<u64>().ok()?.checked_mul(scale)
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
      mounts(&declared),
      "/Users/user/workspace\t/workspace\trw\n/Users/user/.cargo/registry\t/Users/user/.cargo/registry\tro"
    );
  }

  #[test]
  fn writes_no_mounts_as_nothing_at_all() {
    assert_eq!(mounts(&[]), "");
    assert_eq!(lines(&[]), "");
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

    assert_eq!(environment(&declared), "COMPOSTBIN_SPEC_TEST=present\nIS_SANDBOX=1");
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

  #[test]
  fn reads_the_memory_forms_the_manifest_uses() {
    assert_eq!(memory("8G"), Some(8 * 1024 * 1024 * 1024));
    assert_eq!(memory("512M"), Some(512 * 1024 * 1024));
    assert_eq!(memory("1024k"), Some(1024 * 1024));
    assert_eq!(memory("2048"), Some(2048));
  }

  #[test]
  fn reads_no_memory_from_something_that_is_not_a_size() {
    assert_eq!(memory(""), None);
    assert_eq!(memory("lots"), None);
    assert_eq!(memory("8GB"), None);
  }
}
