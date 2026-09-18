//! The two ways a session reaches out of the container: the commands it may run
//! on the host, and the token it authenticates with.

use super::{Check, Status, check, listed};
use crate::host::served;
use crate::session::Session;
use crate::session::credentials::{CREDENTIALS_FILE_NAME, CredentialSource, KEYCHAIN_SERVICE};
use compostbin_engine::engine::Engine;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Loopback answers or refuses at once; this only keeps `doctor` from hanging
/// on a service that accepts nothing.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Lists every allowlisted command, since each runs on the host as the user.
pub fn allowlist(session: &Session) -> Check {
  if !session.manifest.host.has_commands() {
    return check(
      "host commands",
      Status::Ok,
      "no [host.commands]: the guest has no path to the host",
    );
  }

  let served = session.manifest.host.served_commands();
  let commands: Vec<String> = served
    .iter()
    .map(|(name, command)| {
      let widened = if command.arguments { " (+ guest arguments)" } else { "" };
      format!("{name} = {}{widened}", command.argv.join(" "))
    })
    .collect();

  let widened = served.values().any(|command| command.arguments);

  listed(
    "host commands",
    if widened { Status::Warn } else { Status::Ok },
    "these run on the host as you",
    commands,
  )
}

/// Declared ports, and whether anything listens behind each. A down service is
/// a warning, not an error (it may start later), but it is the likeliest reason
/// a guest's `localhost:<port>` refuses.
///
/// Each port is relayed into this container alone, so there is no wider reach
/// to report.
pub fn ports(session: &Session, engine: &impl Engine) -> Check {
  if !session.manifest.host.has_ports() {
    return check("host ports", Status::Ok, "no [host] ports: nothing is forwarded");
  }

  // The container is up and mounted correctly, but the relay holding its
  // sockets has died. The record cannot show this, and rebinding cannot fix it.
  let forwards = session.forwards();

  if session.is_running(engine).unwrap_or(false) && !forwards.iter().all(served) {
    return check(
      "host ports",
      Status::Warn,
      format!(
        "{} is running, but the relay holding its port sockets is gone; exit it and `compostbin run` again",
        session.container_name()
      ),
    );
  }

  let probed: Vec<(u16, bool)> = forwards
    .iter()
    .map(|forward| (forward.port(), answering(forward.upstream)))
    .collect();

  let idle = probed.iter().any(|(_, answering)| !answering);
  let ports = probed
    .iter()
    .map(|(port, answering)| {
      if *answering {
        format!("localhost:{port}")
      } else {
        format!("localhost:{port} — nothing is listening on the host, so it would only refuse")
      }
    })
    .collect();

  listed(
    "host ports",
    if idle { Status::Warn } else { Status::Ok },
    "reachable from this session's container and nothing else",
    ports,
  )
}

fn answering(upstream: SocketAddr) -> bool {
  TcpStream::connect_timeout(&upstream, PROBE_TIMEOUT).is_ok()
}

pub fn credentials(session: &Session, source: &impl CredentialSource, api_key_present: bool) -> Check {
  let seeded = session.claude_home().join(CREDENTIALS_FILE_NAME);

  if seeded.exists() {
    return check("credentials", Status::Ok, seeded.display().to_string());
  }

  // A Keychain token only counts if `run` will copy it in.
  if !session.manifest.claude.seed_from_keychain {
    if api_key_present {
      return check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set");
    }
    return check(
      "credentials",
      Status::Fail,
      "seed_from_keychain is off and no ANTHROPIC_API_KEY; the session cannot authenticate",
    );
  }

  match source.read() {
    Ok(Some(_)) => check("credentials", Status::Ok, format!("{KEYCHAIN_SERVICE} in the Keychain")),
    Ok(None) | Err(_) if api_key_present => check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set"),
    Ok(None) => check(
      "credentials",
      Status::Fail,
      format!("no {KEYCHAIN_SERVICE} entry and no ANTHROPIC_API_KEY; the session cannot authenticate"),
    ),
    Err(error) => check("credentials", Status::Fail, error.to_string()),
  }
}
