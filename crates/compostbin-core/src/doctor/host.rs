//! The two ways a session reaches out of the container: the commands it may run
//! on the host, and the token it authenticates with.

use super::{Check, Status, check, listed};
use crate::host::served;
use crate::session::Session;
use crate::session::credentials::{CREDENTIALS_FILE_NAME, CredentialSource, KEYCHAIN_SERVICE};
use apple_container::engine::Engine;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Loopback answers or refuses at once; this only bounds a service that accepts
/// nothing, and `doctor` must not hang on one.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Every allowlisted command runs on the host with the user's own privileges,
/// so this is where that stops being invisible.
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

/// Declared ports, and whether anything is actually listening behind each one.
/// A forward whose service is down is not an error — the service may start
/// later — but it is the likeliest reason a guest's `localhost:<port>` refuses,
/// and nothing else would say so.
///
/// Reach is not reported because there is none to report: each port travels
/// through a socket in the session directory that `container` relays into this
/// container alone.
pub fn ports(session: &Session, engine: &impl Engine) -> Check {
  if !session.manifest.host.has_ports() {
    return check("host ports", Status::Ok, "no [host] ports: nothing is forwarded");
  }

  // The one failure the record cannot show: the container is up and mounted
  // correctly, and the relay holding the other end of its sockets has died.
  // Nothing rebinds its way back in, so this is the only warning of it.
  if session.is_running(engine).unwrap_or(false) && !session.forwards().iter().all(served) {
    return check(
      "host ports",
      Status::Warn,
      format!(
        "{} is running, but the relay holding its port sockets is gone; `compostbin stop` then `compostbin run`",
        session.container_name()
      ),
    );
  }

  let probed: Vec<(u16, bool)> = session
    .manifest
    .host
    .ports
    .iter()
    .map(|&port| (port, answering(port)))
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

fn answering(port: u16) -> bool {
  TcpStream::connect_timeout(&SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port), PROBE_TIMEOUT).is_ok()
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
