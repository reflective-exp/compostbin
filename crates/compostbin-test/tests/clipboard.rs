#![cfg(feature = "integration")]

use compostbin_core::host::CLIPBOARD_COMMAND;
use compostbin_core::image::CLIPBOARD_TOOLS;
use compostbin_core::session::CLIPBOARD_DISPLAY;
use compostbin_test::{Lock, Project, exclusive, poll_until, stderr, stdout};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const COPY_TIMEOUT: Duration = Duration::from_secs(30);
/// One transparent pixel.
const PNG: [u8; 70] = [
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
  0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
  0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f, 0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba,
  0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
/// The guest's stdout as one hex string, so binary survives the comparison.
const AS_HEX: &str = "od -An -v -tx1 | tr -d ' \\n'";

struct Pasteboard {
  _held: Lock,
  restore: String,
}

impl Pasteboard {
  fn take() -> Self {
    Self {
      _held: exclusive("pasteboard"),
      restore: paste(),
    }
  }
}

impl Drop for Pasteboard {
  /// Runs before the lock field is dropped, so the pasteboard is restored first.
  fn drop(&mut self) {
    copy(&self.restore);
  }
}

fn paste() -> String {
  let output = Command::new("pbpaste")
    .output()
    .expect("pbpaste should run");

  stdout(&output)
}

fn copy(contents: &str) {
  let mut pbcopy = Command::new("pbcopy")
    .stdin(Stdio::piped())
    .spawn()
    .expect("pbcopy should run");
  pbcopy
    .stdin
    .take()
    .expect("piped stdin")
    .write_all(contents.as_bytes())
    .expect("write to the pasteboard");
  pbcopy.wait().expect("pbcopy should finish");
}

/// `class` is the AppleScript class the file is read as, which becomes the pasteboard type.
fn copy_image(path: &Path, class: &str) {
  let output = Command::new("osascript")
    .args([
      "-e",
      "on run argv",
      "-e",
      &format!("set the clipboard to (read (POSIX file (item 1 of argv)) as {class})"),
      "-e",
      "end run",
    ])
    .arg(path)
    .output()
    .expect("osascript should run");

  assert!(output.status.success(), "copying {path:?} failed: {}", stderr(&output));
}

fn hex(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn with_clipboard(name: &str) -> Project {
  let project = Project::new(name);
  project.manifest(
    r#"
[host]
clipboard = true
"#,
  );

  project
}

#[test]
fn every_tool_reaches_the_pasteboard() {
  let project = with_clipboard("cbt-clipboard");
  let _pasteboard = Pasteboard::take();

  let copying: Vec<String> = CLIPBOARD_TOOLS
    .iter()
    .map(|tool| format!("printf '%s' 'copied with {tool}' | {tool} && read step"))
    .collect();
  let mut guest = project.guest_in_background(&copying.join("; "));

  for tool in CLIPBOARD_TOOLS {
    wait_for_the_pasteboard(&format!("copied with {tool}"), tool);
    guest.send("next");
  }

  let finished = guest.finish();
  assert!(
    finished.status.success(),
    "a copying tool failed: {}{}",
    stdout(&finished),
    stderr(&finished)
  );
}

fn wait_for_the_pasteboard(expected: &str, tool: &str) {
  poll_until(COPY_TIMEOUT, &format!("{tool} to reach the host pasteboard"), || {
    (paste() == expected).then_some(())
  });
}

/// Claude on Linux looks for `wl-copy` only when `WAYLAND_DISPLAY` is set.
#[test]
fn session_gets_a_display() {
  let project = with_clipboard("cbt-clipboard-display");

  assert_eq!(project.guest_output("echo $WAYLAND_DISPLAY"), CLIPBOARD_DISPLAY);
}

#[test]
fn pasting_text_is_refused() {
  let _pasteboard = Pasteboard::take();
  let project = with_clipboard("cbt-clipboard-paste");
  copy("a host secret");

  // Not bare `xsel`, which copies rather than pastes when stdin is not a terminal.
  let refused = [
    "xclip -o",
    "xclip -out",
    "xclip -selection clipboard -t text/plain -o",
    "xclip -selection primary -t image/png -o",
    "xsel -o",
    "xsel --output",
    "wl-paste",
    "wl-paste --no-newline",
  ];
  let imageless = [
    "xclip -selection clipboard -t TARGETS -o",
    "xclip -selection clipboard -t image/png -o",
    "wl-paste --list-types",
    "wl-paste --type image/png",
  ];
  let output = project.guest(
    &refused
      .iter()
      .chain(&imageless)
      .map(|attempt| format!("{attempt}; echo \"{attempt} exited $?\""))
      .collect::<Vec<_>>()
      .join("; "),
  );
  let reported = stdout(&output);

  for attempt in refused.iter().chain(&imageless) {
    assert!(
      reported.contains(&format!("{attempt} exited 1")),
      "`{attempt}` should have failed:\n{reported}"
    );
  }
  assert_eq!(
    stderr(&output)
      .lines()
      .filter(|line| line.contains("only an image can be pasted"))
      .count(),
    refused.len(),
    "each refusal should say why: {}",
    stderr(&output)
  );
  assert!(
    !reported.contains("a host secret") && !stderr(&output).contains("a host secret"),
    "text on the host pasteboard may not reach the guest:\n{reported}"
  );
}

/// What Claude runs for Ctrl+V: TARGETS to find an image, then image/png to fetch it.
#[test]
fn pastes_an_image_as_png() {
  let _pasteboard = Pasteboard::take();
  let project = with_clipboard("cbt-clipboard-image");
  let image = project.dir().join("copied.png");
  std::fs::write(&image, PNG).expect("write the image");
  copy_image(&image, "«class PNGf»");

  let output = project.guest_output(&format!(
    "xclip -selection clipboard -t TARGETS -o; wl-paste --list-types; \
     xclip -selection clipboard -t image/png -o | {AS_HEX}; echo; wl-paste --type image/png | {AS_HEX}"
  ));

  assert_eq!(output, format!("image/png\nimage/png\n{0}\n{0}", hex(&PNG)));
}

#[test]
fn pastes_a_tiff_as_png() {
  let _pasteboard = Pasteboard::take();
  let project = with_clipboard("cbt-clipboard-tiff");
  let source = project.dir().join("source.png");
  let image = project.dir().join("copied.tiff");
  std::fs::write(&source, PNG).expect("write the image");
  let converted = Command::new("sips")
    .args(["-s", "format", "tiff"])
    .arg(&source)
    .arg("--out")
    .arg(&image)
    .output()
    .expect("sips should run");
  assert!(converted.status.success(), "{}", stderr(&converted));
  copy_image(&image, "TIFF picture");

  let output = project.guest_output(&format!(
    "xclip -selection clipboard -t TARGETS -o; xclip -selection clipboard -t image/png -o | head -c 8 | {AS_HEX}"
  ));

  assert_eq!(
    output,
    format!("image/png\n{}", hex(&PNG[..8])),
    "a PNG signature, not TIFF data"
  );
}

#[test]
fn off_without_the_setting() {
  let _pasteboard = Pasteboard::take();
  let project = Project::new("cbt-clipboard-off");
  copy("untouched");

  let output = project.guest("printf 'from the guest' | pbcopy");

  assert!(
    !output.status.success(),
    "with no host channel there is nothing to serve it"
  );
  assert_eq!(paste(), "untouched", "and the host pasteboard is not written to");
}

#[test]
fn declared_command_wins() {
  let project = Project::new("cbt-clipboard-own");
  let recorded = project.dir().join("copied");
  project.manifest(&format!(
    r#"
[host]
clipboard = true

[host.commands.{CLIPBOARD_COMMAND}]
argv = ["sh", "-c", "cat > {}"]
"#,
    recorded.display()
  ));

  let output = project.guest("printf 'to the project' | wl-copy");

  assert!(output.status.success(), "{}", stderr(&output));
  assert_eq!(
    std::fs::read_to_string(&recorded).expect("the declared command should have run"),
    "to the project"
  );
}
