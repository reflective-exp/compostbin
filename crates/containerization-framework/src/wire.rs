//! Turning the model into what the bridge carries.
//!
//! Lists cross as newline-separated strings, mounts and sockets as
//! tab-separated fields within them. Both separators are assumed absent from
//! the values: a mount is two paths and a flag, and an argument or environment
//! variable holding a newline would be read as two elements on the far side.
//!
//! A build plan crosses as JSON instead, since a step's script may contain
//! either separator.

use crate::model::{Direction, Mount, SocketRelay};
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

/// `source\tdestination\tmode\tdirection`, the mode in octal.
pub fn sockets(sockets: &[SocketRelay]) -> Vec<String> {
  sockets
    .iter()
    .map(|socket| {
      format!(
        "{}\t{}\t{:o}\t{}",
        socket.source.display(),
        socket.target.display(),
        socket.mode,
        match socket.direction {
          Direction::IntoGuest => "into",
          Direction::OutOfGuest => "outof",
        }
      )
    })
    .collect()
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
  fn writes_a_relayed_socket_with_its_mode_and_direction() {
    let declared = [SocketRelay::into_guest(
      "/state/ports/7001.sock",
      "/run/session/ports/7001.sock",
    )];

    assert_eq!(
      sockets(&declared),
      ["/state/ports/7001.sock\t/run/session/ports/7001.sock\t666\tinto"]
    );
  }

  #[test]
  fn writes_the_direction_a_socket_is_reached_from() {
    let out = SocketRelay {
      direction: Direction::OutOfGuest,
      mode: 0o600,
      ..SocketRelay::into_guest("/host.sock", "/guest.sock")
    };

    assert_eq!(sockets(&[out]), ["/host.sock\t/guest.sock\t600\toutof"]);
  }
}
