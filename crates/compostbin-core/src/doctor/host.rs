use super::{Check, Status, check, listed};
use crate::host::served_commands;
use crate::session::Session;
use crate::session::credentials::{CREDENTIALS_FILE_NAME, CredentialSource};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// Loopback answers at once; this only bounds a service that accepts nothing.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

pub fn allowlist(session: &Session) -> Check {
  if !session.manifest.host.has_commands() {
    return check(
      "host commands",
      Status::Ok,
      "no [host.commands]: the guest has no path to the host",
    );
  }

  let served = served_commands(&session.manifest.host);
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

pub fn ports(session: &Session) -> Check {
  if !session.manifest.host.has_ports() {
    return check("host ports", Status::Ok, "no [host] ports: nothing is forwarded");
  }

  let probed: Vec<(u16, bool)> = session
    .forwards()
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

  if !session.manifest.claude.keychain {
    if api_key_present {
      return check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set");
    }
    return check(
      "credentials",
      Status::Fail,
      "keychain is off and no ANTHROPIC_API_KEY; the session cannot authenticate",
    );
  }

  match source.read() {
    Ok(Some(_)) => check(
      "credentials",
      Status::Ok,
      format!("{} in the Keychain", source.service()),
    ),
    Ok(None) | Err(_) if api_key_present => check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set"),
    Ok(None) => check(
      "credentials",
      Status::Fail,
      format!(
        "no {} entry and no ANTHROPIC_API_KEY; the session cannot authenticate",
        source.service()
      ),
    ),
    Err(error) => check("credentials", Status::Fail, error.to_string()),
  }
}
