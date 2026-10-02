#![cfg(feature = "integration")]
//! `[claude]`: the host's own Claude, copied into the session's home.
//!
//! What the user maintains once — their house style, their settings, their
//! skills — and expects to find in every container. The copy is the host's to
//! make, so the test for it is whether the guest ends up holding it.

use compostbin_core::session::CLAUDE_HOME_TARGET;
use compostbin_core::session::settings::HOST_CLAUDE_SETTINGS;
use compostbin_test::{Project, stdout};

/// The host's `~/.claude`, as a developer's would be: the settings every
/// session gets, and a `widgets` directory only configuration can ask for.
fn with_a_host_home(name: &str) -> Project {
  let project = Project::new(name);
  let home = project.home().join(".claude");

  std::fs::create_dir_all(home.join("skills/mine")).expect("create skills");
  std::fs::create_dir_all(home.join("agents")).expect("create agents");
  std::fs::create_dir_all(home.join("widgets")).expect("create widgets");
  std::fs::write(home.join("CLAUDE.md"), "# house style\n").expect("write CLAUDE.md");
  std::fs::write(home.join("settings.json"), "{}\n").expect("write settings.json");
  std::fs::write(home.join("skills/mine/SKILL.md"), "a skill\n").expect("write a skill");
  std::fs::write(home.join("agents/reviewer.md"), "an agent\n").expect("write an agent");
  std::fs::write(home.join("widgets/sprocket"), "a widget\n").expect("write a widget");

  project
}

#[test]
fn settings_are_copied_in() {
  let project = with_a_host_home("cbt-shared");

  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/CLAUDE.md")),
    "# house style"
  );
  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/skills/mine/SKILL.md")),
    "a skill",
    "a skills directory is copied whole"
  );
  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/agents/reviewer.md")),
    "an agent"
  );
  assert_eq!(
    project.guest_output(&format!("ls {CLAUDE_HOME_TARGET}")),
    HOST_CLAUDE_SETTINGS.join("\n"),
    "and nothing else is shared unless configuration says so"
  );
}

#[test]
fn manifest_can_share_more() {
  let project = with_a_host_home("cbt-shared-extra");
  project.manifest(
    r#"
[claude]
shared = ["widgets"]
"#,
  );

  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/widgets/sprocket")),
    "a widget"
  );
}

/// What the developer wants in every session, named once rather than in each
/// project's manifest.
#[test]
fn user_config_can_share_more() {
  let project = with_a_host_home("cbt-shared-user");
  let config = project.home().join(".config/compostbin");
  std::fs::create_dir_all(&config).expect("create config dir");
  std::fs::write(config.join("config.toml"), "[claude]\nshared = [\"widgets\"]\n").expect("write config");

  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/widgets/sprocket")),
    "a widget"
  );
}

/// The host copy is authoritative, so a shared directory is replaced rather
/// than merged: a skill deleted on the host disappears from the session too.
#[test]
fn host_copy_wins() {
  let project = with_a_host_home("cbt-shared-host");
  project.guest_output(&format!("mkdir -p {CLAUDE_HOME_TARGET}/skills/stale"));

  std::fs::remove_dir_all(project.home().join(".claude/skills/mine")).expect("delete the skill");
  std::fs::create_dir_all(project.home().join(".claude/skills/replaced")).expect("create another");

  assert_eq!(
    project.guest_output(&format!("ls {CLAUDE_HOME_TARGET}/skills")),
    "replaced",
    "what the session had is replaced by what the host has now"
  );
}

/// Copying into the session's home happens on the host, before the container:
/// the notice is what tells the user their settings went in.
#[test]
fn says_what_it_shared() {
  let project = with_a_host_home("cbt-shared-notice");

  let output = project.guest("true");

  assert!(
    stdout(&output).contains(&format!(
      "shared from your own ~/.claude: {}",
      HOST_CLAUDE_SETTINGS.join(", ")
    )),
    "stdout: {}",
    stdout(&output)
  );
}

/// Nothing to share is ordinary — a fresh machine, or CI — and must not stop a
/// session starting.
#[test]
fn no_host_home_still_works() {
  let project = Project::new("cbt-shared-none");

  assert_eq!(project.guest_output("echo started"), "started");
  assert!(
    project.state_dir().join("claude-home").is_dir(),
    "the home is a mount source, so it exists whether or not anything was put in it"
  );
}
