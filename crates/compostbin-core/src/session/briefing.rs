//! Uses managed settings because `settings::share` overwrites `settings.json` from the host.

use crate::error::{At, PathError};
use crate::host::{CLIPBOARD_COMMAND, GUEST_SOCKETS_GROUP, served_commands};
use crate::manifest::Manifest;
use std::path::Path;

/// Fixed by Claude Code on Linux.
pub const MANAGED_SETTINGS_TARGET: &str = "/etc/claude-code";
/// Relative to the session state directory.
pub const MANAGED_SETTINGS_DIR: &str = "managed";
pub const MANAGED_SETTINGS_FILE: &str = "managed-settings.json";
pub const BRIEFING_FILE: &str = "session-context.txt";

/// Claude adds a `SessionStart` hook's stdout to its context. The hook drains its stdin first:
/// exiting with Claude's payload unread can fail Claude's write with EPIPE.
pub fn managed_settings() -> String {
  format!(
    r#"{{
  "hooks": {{
    "SessionStart": [
      {{
        "hooks": [
          {{
            "type": "command",
            "command": "cat >/dev/null; cat {MANAGED_SETTINGS_TARGET}/{BRIEFING_FILE}"
          }}
        ]
      }}
    ]
  }}
}}
"#
  )
}

pub fn briefing(manifest: &Manifest, config: &str) -> String {
  let mut text = String::from(
    "This session is running inside a compostbin container. It is a Debian guest \
     with no project toolchain installed: only the host paths mounted under /workspace \
     are visible, and edits to them land directly on the host.\n\n",
  );

  if !manifest.host.has_commands() {
    text.push_str(&format!(
      "This project declares no host commands, so there is no path out to the host \
       at all. Anything that has to run, runs in the guest — or gets a [host.commands] \
       entry in {config}.\n"
    ));
    text.push_str(&ports(manifest, config));
    text.push_str(&sockets(manifest, config));

    return text;
  }

  text.push_str(
    "Builds and tests run on the host, through `compostbin-host <name>`. It behaves as \
     though this shell had run the command: stdin goes to it, stdout and stderr come back \
     on ours, and its exit status is ours. Prefer it over running the underlying tool here, \
     which is not installed.\n\nThis project declares:\n\n",
  );

  let commands = served_commands(&manifest.host);
  let width = commands.keys().map(String::len).max().unwrap_or_default();

  for (name, command) in commands.iter() {
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
     [host.commands] in {config} — which takes a restart to serve.\n"
  ));

  if manifest.host.clipboard {
    text.push_str(&format!(
      "\nCopying to the clipboard reaches the user's macOS clipboard: `pbcopy`, `xclip`, `xsel` \
       and `wl-copy` here all send what they read to `compostbin-host {CLIPBOARD_COMMAND}`. \
       It is write-only; nothing can be pasted from the host.\n"
    ));
  }

  text.push_str(&ports(manifest, config));
  text.push_str(&sockets(manifest, config));

  text
}

fn ports(manifest: &Manifest, config: &str) -> String {
  if !manifest.host.has_ports() {
    return String::new();
  }

  let listed: Vec<String> = manifest
    .host
    .ports
    .iter()
    .map(|port| format!("localhost:{port}"))
    .collect();

  format!(
    "\nSome of the host's own services answer here, at the same address they have on \
     the host: {}. They are the host's, not this container's, and they are fixed when \
     the container is created — changing [host] ports in {config} takes \
     a restart.\n",
    listed.join(", ")
  )
}

fn sockets(manifest: &Manifest, config: &str) -> String {
  if !manifest.host.has_sockets() {
    return String::new();
  }

  let listed: Vec<String> = manifest
    .host
    .socket_targets()
    .map(|(target, _)| target.display().to_string())
    .collect();

  format!(
    "\nSome of the host's Unix sockets answer here: {}. They are fixed when the container \
     is created — changing [host.sockets] in {config} takes a restart. You reach them through \
     the {GUEST_SOCKETS_GROUP} group. An MCP server behind one is a stdio server run as \
     `socat - UNIX-CONNECT:<socket>`.\n",
    listed.join(", ")
  )
}

/// Must run before the container starts, since the mount source has to exist.
pub fn write(dir: &Path, manifest: &Manifest, config: &str) -> Result<(), PathError> {
  std::fs::create_dir_all(dir).at(dir)?;

  for (name, contents) in [
    (MANAGED_SETTINGS_FILE, managed_settings()),
    (BRIEFING_FILE, briefing(manifest, config)),
  ] {
    let path = dir.join(name);
    std::fs::write(&path, contents).at(&path)?;
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::manifest::{MANIFEST_RELATIVE_PATH, profile_path};
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
      settings.contains("\"command\": \"cat >/dev/null; cat /etc/claude-code/session-context.txt\""),
      "the hook must print the file the same mount carries: {settings}"
    );
  }

  #[test]
  fn the_briefing_names_the_container() {
    let briefing = briefing(&Manifest::default(), MANIFEST_RELATIVE_PATH);

    assert!(
      briefing.contains("compostbin container"),
      "a session that does not know where it is cannot act on the rest: {briefing}"
    );
  }

  #[test]
  fn the_briefing_lists_every_declared_command() {
    let briefing = briefing(&manifest_with_commands(), MANIFEST_RELATIVE_PATH);

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
    let briefing = briefing(&manifest_with_commands(), MANIFEST_RELATIVE_PATH);

    assert!(
      briefing.contains("takes further arguments"),
      "a guest told nothing appends arguments to a command that refuses them: {briefing}"
    );
    assert!(briefing.contains("when your stdout is a terminal"), "{briefing}");
  }

  #[test]
  fn the_briefing_says_so_when_there_is_no_channel() {
    let briefing = briefing(&Manifest::default(), MANIFEST_RELATIVE_PATH);

    assert!(
      !briefing.contains("compostbin-host <name>"),
      "there is no host channel to point at: {briefing}"
    );
    assert!(briefing.contains("no host commands"), "{briefing}");
  }

  #[test]
  fn the_briefing_says_where_copies_go() {
    let manifest: Manifest = toml::from_str("[host]\nclipboard = true\n").expect("manifest should parse");
    let briefing = briefing(&manifest, MANIFEST_RELATIVE_PATH);

    assert!(briefing.contains("compostbin-host clipboard  pbcopy"), "{briefing}");
    assert!(briefing.contains("macOS clipboard"), "{briefing}");
    assert!(!super::briefing(&manifest_with_commands(), MANIFEST_RELATIVE_PATH).contains("clipboard"));
  }

  #[test]
  fn the_briefing_lists_each_socket_where_the_guest_finds_it() {
    let manifest: Manifest = toml::from_str(
      "[host.sockets.mcp]\nsource = \"~/mcp.sock\"\n\n[host.sockets.other]\nsource = \"/other.sock\"\ntarget = \"/run/other.sock\"\n",
    )
    .expect("manifest should parse");

    for briefing in [
      briefing(&manifest, MANIFEST_RELATIVE_PATH),
      briefing(
        &Manifest {
          host: crate::manifest::HostConfig {
            sockets: manifest.host.sockets.clone(),
            ..manifest_with_commands().host
          },
          ..Manifest::default()
        },
        MANIFEST_RELATIVE_PATH,
      ),
    ] {
      assert!(
        briefing.contains("/run/compostbin/sockets/mcp, /run/other.sock"),
        "with or without host commands: {briefing}"
      );
      assert!(briefing.contains("socat - UNIX-CONNECT:"), "{briefing}");
      assert!(
        !briefing.contains("~/mcp.sock"),
        "the host path means nothing here: {briefing}"
      );
    }
    assert!(!super::briefing(&manifest_with_commands(), MANIFEST_RELATIVE_PATH).contains("Unix sockets"));
  }

  #[test]
  fn the_briefing_names_the_profile() {
    let briefing = briefing(&manifest_with_commands(), &profile_path("rust"));

    assert!(
      briefing.contains("[host.commands] in ~/.config/compostbin/profiles/rust.toml"),
      "{briefing}"
    );
    assert!(!briefing.contains(MANIFEST_RELATIVE_PATH), "{briefing}");
  }

  #[test]
  fn writes_both_files() {
    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join("managed");

    write(&target, &manifest_with_commands(), MANIFEST_RELATIVE_PATH).expect("writing should succeed");

    assert_eq!(
      std::fs::read_to_string(target.join(MANAGED_SETTINGS_FILE)).expect("settings should exist"),
      managed_settings()
    );
    assert_eq!(
      std::fs::read_to_string(target.join(BRIEFING_FILE)).expect("the briefing should exist"),
      briefing(&manifest_with_commands(), MANIFEST_RELATIVE_PATH)
    );
  }
}
