//! What the guest asked for, and whether it is allowed.

use crate::error::Refusal;
use crate::manifest::HostCommand;
use std::collections::BTreeMap;

/// The name of an allowlisted command, plus any arguments the guest appended.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
  pub arguments: Vec<String>,
  pub command: String,
}

impl Request {
  pub fn new(command: impl Into<String>, arguments: Vec<String>) -> Self {
    Self {
      arguments,
      command: command.into(),
    }
  }

  /// One field per line, command first: the writer is a shell script, and what
  /// `printf '%s\n'` emits has no escaping to get wrong. The cost is that an
  /// argument may not contain a newline.
  pub fn render(&self) -> Result<String, Refusal> {
    for field in std::iter::once(&self.command).chain(self.arguments.iter()) {
      if field.contains('\n') {
        return Err(Refusal::NewlineInArgument(field.clone()));
      }
    }

    let mut rendered = self.command.clone();
    for argument in &self.arguments {
      rendered.push('\n');
      rendered.push_str(argument);
    }
    rendered.push('\n');

    Ok(rendered)
  }

  pub fn parse(text: &str) -> Result<Self, Refusal> {
    let mut lines = text.lines();
    let command = lines.next().unwrap_or_default().to_string();

    if command.is_empty() {
      return Err(Refusal::EmptyRequest);
    }

    Ok(Self {
      arguments: lines.map(str::to_string).collect(),
      command,
    })
  }
}

/// The argv to run, or why the request is refused. Pure, so the allowlist
/// decision is testable without a filesystem or a container.
pub fn resolve(commands: &BTreeMap<String, HostCommand>, request: &Request) -> Result<Vec<String>, Refusal> {
  let Some(command) = commands.get(&request.command) else {
    return Err(Refusal::UnknownCommand(request.command.clone()));
  };

  if command.argv.is_empty() {
    return Err(Refusal::EmptyCommand(request.command.clone()));
  }

  if !request.arguments.is_empty() {
    if !command.arguments {
      return Err(Refusal::ArgumentsNotAllowed(request.command.clone()));
    }

    if let Some(denied) = request
      .arguments
      .iter()
      .find(|argument| is_denied(&command.deny, argument))
    {
      return Err(Refusal::DeniedArgument(denied.clone()));
    }
  }

  let mut argv = command.argv.clone();
  argv.extend(request.arguments.iter().cloned());
  Ok(argv)
}

/// This does not stop the guest running code on the host: it can edit the repo,
/// and a command that builds the repo runs whatever is there. It stops a guest
/// argument from swapping in configuration the user never reviewed. Matches
/// every spelling that reaches the same place — `--config` and `--config=x`, and
/// for a single-letter flag the joined `-Zx` too.
fn is_denied(deny: &[String], argument: &str) -> bool {
  deny.iter().any(|denied| {
    let short = denied.len() == 2 && denied.starts_with('-') && denied != "--";
    argument == denied
      || argument.starts_with(&format!("{denied}="))
      || (short && argument.starts_with(denied.as_str()))
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::host::fixtures::{self, allowlist};

  #[test]
  fn resolves_an_allowlisted_command_to_its_manifest_argv() {
    assert_eq!(
      resolve(&allowlist(), &Request::new("test", Vec::new())).expect("should resolve"),
      ["cargo", "nextest", "run", "--workspace"]
    );
  }

  #[test]
  fn refuses_a_command_that_is_not_on_the_allowlist() {
    assert_eq!(
      resolve(&allowlist(), &Request::new("rm", vec!["-rf".to_string()])),
      Err(Refusal::UnknownCommand("rm".to_string()))
    );
  }

  /// The per-command flag: `test` is exact, `test-one` is not.
  #[test]
  fn refuses_arguments_unless_the_command_opted_in() {
    let filter = vec!["my_test".to_string()];

    assert_eq!(
      resolve(&allowlist(), &Request::new("test", filter.clone())),
      Err(Refusal::ArgumentsNotAllowed("test".to_string()))
    );
    assert_eq!(
      resolve(&allowlist(), &Request::new("test-one", filter)).expect("should resolve"),
      ["cargo", "nextest", "run", "my_test"]
    );
  }

  #[test]
  fn refuses_arguments_that_redirect_the_command_elsewhere() {
    for argument in [
      "--config",
      "--config=target.runner='sh -c'",
      "--manifest-path",
      "-Z",
      "-Z=build-std",
      "-Zbuild-std",
    ] {
      assert_eq!(
        resolve(&allowlist(), &Request::new("test-one", vec![argument.to_string()])),
        Err(Refusal::DeniedArgument(argument.to_string())),
        "{argument} should be refused"
      );
    }
  }

  /// A denied prefix must not swallow an argument that merely starts the same.
  #[test]
  fn allows_an_argument_that_only_looks_like_a_denied_one() {
    assert_eq!(
      resolve(
        &allowlist(),
        &Request::new("test-one", vec!["--configured".to_string()])
      )
      .expect("should resolve"),
      ["cargo", "nextest", "run", "--configured"]
    );
  }

  /// Nothing is denied that the manifest did not name: `-Z` means something else
  /// to another toolchain.
  #[test]
  fn denies_only_what_the_command_declares() {
    let commands = fixtures::commands(&[("run", &["tool"], true)]);

    assert_eq!(
      resolve(&commands, &Request::new("run", vec!["-Zanything".to_string()])).expect("should resolve"),
      ["tool", "-Zanything"]
    );
  }

  /// `--` is two characters but not a single-letter flag, so denying it must not
  /// deny every long flag.
  #[test]
  fn denying_the_separator_denies_only_the_separator() {
    let mut commands = fixtures::commands(&[("run", &["tool"], true)]);
    commands
      .get_mut("run")
      .expect("the command was just inserted")
      .deny = vec!["--".to_string()];

    assert_eq!(
      resolve(&commands, &Request::new("run", vec!["--".to_string()])),
      Err(Refusal::DeniedArgument("--".to_string()))
    );

    assert_eq!(
      resolve(&commands, &Request::new("run", vec!["--verbose".to_string()])).expect("should resolve"),
      ["tool", "--verbose"]
    );
  }

  #[test]
  fn round_trips_a_request_through_its_wire_format() {
    let request = Request::new("test-one", vec!["-p".to_string(), "compostbin-core".to_string()]);
    let rendered = request.render().expect("should render");

    assert_eq!(rendered, "test-one\n-p\ncompostbin-core\n");
    assert_eq!(Request::parse(&rendered).expect("should parse"), request);
  }

  /// A newline would parse as a further argument, so it is refused at the writer.
  #[test]
  fn refuses_to_render_an_argument_containing_a_newline() {
    let argument = "one\ntwo".to_string();

    assert_eq!(
      Request::new("test-one", vec![argument.clone()]).render(),
      Err(Refusal::NewlineInArgument(argument))
    );
  }
}
