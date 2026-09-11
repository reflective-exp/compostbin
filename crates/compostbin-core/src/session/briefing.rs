//! What a session is told about itself, mounted read-only as Claude's managed
//! settings.
//!
//! A container looks like an ordinary Debian box from the inside: nothing about
//! `/workspace` says the toolchain is missing on purpose, or that `compostbin-host`
//! is the way out to the host. So the session says it, on every start, from the
//! manifest — generated rather than written by hand, because prose repeating an
//! allowlist drifts from it.
//!
//! Managed settings rather than `settings.json`, which `settings::share` copies
//! from the host's `~/.claude` and would overwrite; that file is the user's, and
//! this is not.

use crate::error::PathError;
use crate::manifest::{MANIFEST_RELATIVE_PATH, Manifest};
use std::path::Path;

/// Claude's managed settings directory on Linux. Fixed by Claude Code, not by
/// us: the guest reads this path or nothing.
pub const MANAGED_SETTINGS_TARGET: &str = "/etc/claude-code";
/// The generated pair, under the session state directory.
pub const MANAGED_SETTINGS_DIR: &str = "managed";
pub const MANAGED_SETTINGS_FILE: &str = "managed-settings.json";
/// Beside the settings that name it, so one mount carries both.
pub const BRIEFING_FILE: &str = "session-context.txt";

/// A `SessionStart` hook's stdout is added to Claude's context, so the briefing
/// is a file the hook prints rather than anything the guest has to find.
pub fn managed_settings() -> String {
  format!(
    r#"{{
  "hooks": {{
    "SessionStart": [
      {{
        "hooks": [
          {{
            "type": "command",
            "command": "cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"
          }}
        ]
      }}
    ]
  }}
}}
"#
  )
}

/// The briefing itself: where the session is, and every host command it may ask
/// for, rendered from the manifest that serves them.
pub fn briefing(manifest: &Manifest) -> String {
  let mut text = String::from(
    "This session is running inside a compostbin container. It is a Debian guest \
     with no project toolchain installed: only the host paths mounted under /workspace \
     are visible, and edits to them land directly on the host.\n\n",
  );

  if !manifest.host.has_commands() {
    text.push_str(&format!(
      "This project declares no host commands, so there is no path out to the host \
       at all. Anything that has to run, runs in the guest — or gets a [host.commands] \
       entry in {MANIFEST_RELATIVE_PATH}.\n"
    ));

    return text;
  }

  text.push_str(
    "Builds and tests run on the host, through `compostbin-host <name>`. It behaves as \
     though this shell had run the command: stdin goes to it, stdout and stderr come back \
     on ours, and its exit status is ours. Prefer it over running the underlying tool here, \
     which is not installed.\n\nThis project declares:\n\n",
  );

  let width = manifest
    .host
    .commands
    .keys()
    .map(String::len)
    .max()
    .unwrap_or_default();

  for (name, command) in &manifest.host.commands {
    // The argv is what actually runs, and knowing it is what makes the name
    // mean something; the notes are the two ways a command differs from exact.
    let mut notes = Vec::new();
    if command.arguments {
      notes.push("takes further arguments");
    }
    if command.tty {
      notes.push("runs under a tty when your stdout is a terminal, merging stderr into stdout");
    }
    let notes = if notes.is_empty() {
      String::new()
    } else {
      format!("  ({})", notes.join("; "))
    };

    text.push_str(&format!(
      "  compostbin-host {name:width$}  {}{notes}\n",
      command.argv.join(" ")
    ));
  }

  text.push_str(&format!(
    "\nA command not on that list has no host path. Run it in the guest, or add it to \
     [host.commands] in {MANIFEST_RELATIVE_PATH} — which takes a restart to serve.\n"
  ));

  text
}

/// Writes both files into `dir`, creating it. Called before the container
/// starts, since the mount source must exist by then and the guest cannot
/// create it.
pub fn write(dir: &Path, manifest: &Manifest) -> Result<(), PathError> {
  std::fs::create_dir_all(dir).map_err(|source| PathError::new(dir, source))?;

  for (name, contents) in [
    (MANAGED_SETTINGS_FILE, managed_settings()),
    (BRIEFING_FILE, briefing(manifest)),
  ] {
    let path = dir.join(name);
    std::fs::write(&path, contents).map_err(|source| PathError::new(&path, source))?;
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn manifest_with_commands() -> Manifest {
    toml::from_str(
      "[host.commands.test]\nargv = [\"cargo\", \"nextest\", \"run\", \"--workspace\"]\n\
       [host.commands.test-one]\nargv = [\"cargo\", \"nextest\", \"run\"]\narguments = true\ntty = true\n",
    )
    .expect("manifest should parse")
  }

  #[test]
  fn managed_settings_run_the_briefing_at_session_start() {
    let settings = managed_settings();

    assert!(settings.contains("\"SessionStart\""), "{settings}");
    assert!(
      settings.contains("\"command\": \"cat /etc/claude-code/session-context.txt\""),
      "the hook must print the file the same mount carries: {settings}"
    );
  }

  #[test]
  fn the_briefing_names_the_container() {
    let briefing = briefing(&Manifest::default());

    assert!(
      briefing.contains("compostbin container"),
      "a session that does not know where it is cannot act on the rest: {briefing}"
    );
  }

  #[test]
  fn the_briefing_lists_every_declared_command() {
    let briefing = briefing(&manifest_with_commands());

    assert!(
      briefing.contains("compostbin-host test      cargo nextest run --workspace"),
      "the name is what the guest sends, the argv is what it means: {briefing}"
    );
    assert!(
      briefing.contains("compostbin-host test-one  cargo nextest run"),
      "{briefing}"
    );
  }

  #[test]
  fn the_briefing_marks_a_widened_command() {
    let briefing = briefing(&manifest_with_commands());

    assert!(
      briefing.contains("takes further arguments"),
      "a guest told nothing appends arguments to a command that refuses them: {briefing}"
    );
    assert!(briefing.contains("when your stdout is a terminal"), "{briefing}");
  }

  /// The default manifest declares no commands, and telling a session to use a
  /// channel it does not have is worse than saying nothing.
  #[test]
  fn the_briefing_says_so_when_there_is_no_channel() {
    let briefing = briefing(&Manifest::default());

    assert!(
      !briefing.contains("compostbin-host <name>"),
      "there is no host channel to point at: {briefing}"
    );
    assert!(briefing.contains("no host commands"), "{briefing}");
  }

  #[test]
  fn writes_both_files() {
    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join("managed");

    write(&target, &manifest_with_commands()).expect("writing should succeed");

    assert_eq!(
      std::fs::read_to_string(target.join(MANAGED_SETTINGS_FILE)).expect("settings should exist"),
      managed_settings()
    );
    assert_eq!(
      std::fs::read_to_string(target.join(BRIEFING_FILE)).expect("the briefing should exist"),
      briefing(&manifest_with_commands())
    );
  }
}
