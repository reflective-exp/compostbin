//! The two ways a session reaches back out of the container: the commands it may
//! run on the host, and the token it authenticates to Anthropic with.

use super::{Check, Status, check};
use crate::session::Session;
use crate::session::credentials::{CREDENTIALS_FILE_NAME, CredentialSource, KEYCHAIN_SERVICE};

/// Every allowlisted command runs on the host with the user's own privileges
/// (D6), so this is where that stops being invisible.
pub fn allowlist(session: &Session) -> Check {
  if session.manifest.host.is_empty() {
    return check(
      "host commands",
      Status::Ok,
      "no [host.commands]: the guest has no path to the host",
    );
  }

  let listed: Vec<String> = session
    .manifest
    .host
    .commands
    .iter()
    .map(|(name, command)| {
      let widened = if command.arguments { " (+ guest arguments)" } else { "" };
      format!("{name} = {}{widened}", command.argv.join(" "))
    })
    .collect();

  let widened = session
    .manifest
    .host
    .commands
    .values()
    .any(|command| command.arguments);

  check(
    "host commands",
    if widened { Status::Warn } else { Status::Ok },
    format!("run on the host as you: {}", listed.join("; ")),
  )
}

pub fn credentials(session: &Session, source: &impl CredentialSource, api_key_present: bool) -> Check {
  let seeded = session.claude_home().join(CREDENTIALS_FILE_NAME);

  if seeded.exists() {
    return check("credentials", Status::Ok, seeded.display().to_string());
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
