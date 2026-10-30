use super::{Check, Status, check, listed};
use crate::host::{Forward, Upstream, served_commands};
use crate::session::Session;
use crate::session::credentials::{self, CredentialStore, CredentialsFile};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
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

  forwarded("host ports", session, |forward| match forward.upstream {
    Upstream::Tcp(_) => Some(forward.guest_address()),
    Upstream::Unix(_) => None,
  })
}

pub fn sockets(session: &Session) -> Check {
  if !session.manifest.host.has_sockets() {
    return check("host sockets", Status::Ok, "no [host] sockets: nothing is forwarded");
  }

  forwarded("host sockets", session, |forward| match &forward.upstream {
    Upstream::Unix(path) => Some(format!("{} → {}", forward.guest_address(), path.display())),
    Upstream::Tcp(_) => None,
  })
}

/// The forwards `label` names, each probed on the host.
fn forwarded(name: &'static str, session: &Session, label: impl Fn(&Forward) -> Option<String>) -> Check {
  let probed: Vec<(String, bool)> = session
    .forwards()
    .iter()
    .filter_map(|forward| Some((label(forward)?, answering(&forward.upstream))))
    .collect();

  let idle = probed.iter().any(|(_, answering)| !answering);
  let items = probed
    .into_iter()
    .map(|(label, answering)| {
      if answering {
        label
      } else {
        format!("{label} — nothing is listening on the host, so it would only refuse")
      }
    })
    .collect();

  listed(
    name,
    if idle { Status::Warn } else { Status::Ok },
    "reachable from this session's container and nothing else",
    items,
  )
}

fn answering(upstream: &Upstream) -> bool {
  match upstream {
    Upstream::Tcp(address) => TcpStream::connect_timeout(address, PROBE_TIMEOUT).is_ok(),
    Upstream::Unix(path) => UnixStream::connect(path).is_ok(),
  }
}

pub fn credentials(session: &Session, source: &impl CredentialStore, api_key_present: bool) -> Check {
  let seeded = CredentialsFile::from(session.claude_home().as_path());

  if credentials::has_login(&seeded).unwrap_or(false) {
    return check("credentials", Status::Ok, seeded.name());
  }

  if !session.user.claude.keychain {
    if api_key_present {
      return check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set");
    }
    return check(
      "credentials",
      Status::Fail,
      "keychain is off and no ANTHROPIC_API_KEY; the session cannot authenticate",
    );
  }

  match credentials::has_login(source) {
    Ok(true) => check("credentials", Status::Ok, format!("{} in the Keychain", source.name())),
    Ok(false) | Err(_) if api_key_present => check("credentials", Status::Ok, "ANTHROPIC_API_KEY is set"),
    Ok(false) => check(
      "credentials",
      Status::Fail,
      format!(
        "no login in {} and no ANTHROPIC_API_KEY; the session cannot authenticate",
        source.name()
      ),
    ),
    Err(error) => check("credentials", Status::Fail, error.to_string()),
  }
}
