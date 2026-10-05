//! Attaching a process to a running session.

use crate::engine::select;
use compostbin_core::session::credentials::{KEYCHAIN_SERVICE, Keychain};
use compostbin_core::session::{Notice, Process, Session};
use std::error::Error;
use std::io::IsTerminal;

fn notify(notice: Notice) {
  match notice {
    Notice::NotInKeychain => eprintln!(
      "no \"{KEYCHAIN_SERVICE}\" entry in the login Keychain; the session will need ANTHROPIC_API_KEY or an interactive login"
    ),
    Notice::Shared(names) => eprintln!("shared from ~/.claude: {}", names.join(", ")),
    Notice::Port(event) => eprintln!("compostbin: {event}"),
    Notice::Unpacking(image) => eprintln!("compostbin: unpacking {image}"),
    Notice::SetupFailed { line, code } => eprintln!("compostbin: setup `{line}` exited {code}"),
    Notice::AgentStopped(error) => eprintln!("compostbin: the host command agent stopped: {error}"),
    Notice::CleanupFailed(error) => eprintln!("compostbin: could not clean up after the session: {error}"),
  }
}

/// A terminal asked for is a terminal required: this side can only pass on one
/// it has, and silently running without would leave whatever wanted it — a
/// shell, anything drawing a UI — with pipes and no way to say so. Says so when
/// there is none.
pub fn missing_terminal() -> bool {
  let missing = !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());

  if missing {
    eprintln!("compostbin: needs a terminal on stdin and stdout");
  }

  missing
}

/// Attaches `process` to the session, creating the container if it isn't up.
pub fn attach(session: &Session, process: &Process) -> Result<i32, Box<dyn Error>> {
  Ok(session.run(&select(session)?, &Keychain, process, &notify)?)
}
