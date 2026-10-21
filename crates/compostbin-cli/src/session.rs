use crate::engine::select;
use compostbin_core::session::credentials::{CredentialSource, Keychain};
use compostbin_core::session::{Notice, Process, Session};
use compostbin_core::workspace::paths::CLAUDE_CONFIG_DIR;
use std::error::Error;
use std::io::IsTerminal;

pub fn keychain() -> Keychain {
  Keychain::from(std::env::var_os(CLAUDE_CONFIG_DIR).as_deref())
}

fn notify(keychain: &Keychain, notice: Notice) {
  match notice {
    Notice::NotInKeychain => eprintln!(
      "no \"{}\" entry in the login Keychain; the session will need ANTHROPIC_API_KEY or an interactive login",
      keychain.service()
    ),
    Notice::Shared(names) => eprintln!("shared from the host's Claude home: {}", names.join(", ")),
    Notice::Port(event) => eprintln!("compostbin: {event}"),
    Notice::Unpacking(image) => eprintln!("compostbin: unpacking {image}"),
    Notice::SetupFailed { line, code } => eprintln!("compostbin: setup `{line}` exited {code}"),
    Notice::AgentStopped(error) => eprintln!("compostbin: the host command agent stopped: {error}"),
    Notice::CleanupFailed(error) => eprintln!("compostbin: could not clean up after the session: {error}"),
  }
}

/// Prints an error when stdin or stdout is not a terminal.
pub fn missing_terminal() -> bool {
  let missing = !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());

  if missing {
    eprintln!("compostbin: needs a terminal on stdin and stdout");
  }

  missing
}

pub fn attach(session: &Session, process: &Process) -> Result<i32, Box<dyn Error>> {
  let keychain = keychain();
  Ok(session.run(&select(session)?, &keychain, process, &|notice| {
    notify(&keychain, notice)
  })?)
}
