//! Claude skills to be installed into the host's `~/.claude/skills`.
//! Sessions gain access via the sessions bind mounts.

use crate::error::{At, PathError};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct Skill {
  pub name: &'static str,
  /// Paths relative to the skill's directory, and their contents.
  pub files: &'static [(&'static str, &'static str)],
}

pub const SKILLS: &[Skill] = &[Skill {
  name: "compostbin-manifest",
  files: &[
    ("SKILL.md", include_str!("compostbin-manifest/SKILL.md")),
    (
      "references/claude.md",
      include_str!(concat!(env!("COMPOSTBIN_DOCS_DIR"), "/manifest/claude.md")),
    ),
    (
      "references/container.md",
      include_str!(concat!(env!("COMPOSTBIN_DOCS_DIR"), "/manifest/container.md")),
    ),
    (
      "references/host.md",
      include_str!(concat!(env!("COMPOSTBIN_DOCS_DIR"), "/manifest/host.md")),
    ),
    (
      "references/mounts.md",
      include_str!(concat!(env!("COMPOSTBIN_DOCS_DIR"), "/manifest/mounts.md")),
    ),
  ],
}];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
  Create,
  Update,
  /// In a skill's directory but no longer shipped.
  Remove,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Change {
  pub action: Action,
  pub path: PathBuf,
  contents: Option<&'static str>,
}

/// What installing into `skills_dir` would change. Files already current are
/// left out, so an empty plan means up to date.
pub fn plan(skills_dir: &Path) -> Result<Vec<Change>, PathError> {
  let mut changes = Vec::new();

  for skill in SKILLS {
    let dir = skills_dir.join(skill.name);
    let shipped: BTreeSet<PathBuf> = skill.files.iter().map(|(name, _)| dir.join(name)).collect();

    for (name, contents) in skill.files {
      let path = dir.join(name);
      let action = match std::fs::read(&path).at(&path) {
        Ok(existing) if existing == contents.as_bytes() => continue,
        Ok(_) => Action::Update,
        Err(error) if error.is_not_found() => Action::Create,
        Err(error) => return Err(error),
      };
      changes.push(Change {
        action,
        path,
        contents: Some(contents),
      });
    }

    for path in files_under(&dir)? {
      if !shipped.contains(&path) {
        changes.push(Change {
          action: Action::Remove,
          path,
          contents: None,
        });
      }
    }
  }

  Ok(changes)
}

pub fn apply(changes: &[Change]) -> Result<(), PathError> {
  for change in changes {
    match change.contents {
      Some(contents) => {
        if let Some(parent) = change.path.parent() {
          std::fs::create_dir_all(parent).at(parent)?;
        }
        std::fs::write(&change.path, contents).at(&change.path)?;
      }
      None => std::fs::remove_file(&change.path).at(&change.path)?,
    }
  }

  Ok(())
}

/// Every file below `dir`, sorted; none when `dir` does not exist yet.
fn files_under(dir: &Path) -> Result<Vec<PathBuf>, PathError> {
  let entries = match std::fs::read_dir(dir).at(dir) {
    Ok(entries) => entries,
    Err(error) if error.is_not_found() => return Ok(Vec::new()),
    Err(error) => return Err(error),
  };

  let mut files = Vec::new();
  for entry in entries {
    let path = entry.at(dir)?.path();
    if path.is_dir() {
      files.extend(files_under(&path)?);
    } else {
      files.push(path);
    }
  }
  files.sort();

  Ok(files)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::manifest::{
    ClaudeConfig, ContainerConfig, HostCommand, HostConfig, ImageConfig, Manifest, Memory, PathEntry, ProjectConfig,
    WorkspaceConfig,
  };
  use tempfile::TempDir;

  fn manifest_skill() -> &'static Skill {
    SKILLS
      .iter()
      .find(|skill| skill.name == "compostbin-manifest")
      .expect("the manifest skill should ship")
  }

  #[test]
  fn installs_every_file_into_a_fresh_directory() {
    let temp = TempDir::new().expect("temp dir");

    let changes = plan(temp.path()).expect("plan");
    assert!(changes.iter().all(|change| change.action == Action::Create));

    apply(&changes).expect("apply");

    for (name, contents) in manifest_skill().files {
      let path = temp.path().join("compostbin-manifest").join(name);
      assert_eq!(std::fs::read_to_string(&path).expect("installed"), *contents);
    }
    assert_eq!(plan(temp.path()).expect("plan"), [], "installed is up to date");
  }

  #[test]
  fn updates_a_changed_file_and_removes_one_no_longer_shipped() {
    let temp = TempDir::new().expect("temp dir");
    apply(&plan(temp.path()).expect("plan")).expect("apply");
    let dir = temp.path().join("compostbin-manifest");
    std::fs::write(dir.join("SKILL.md"), "edited").expect("edit");
    std::fs::write(dir.join("references/retired.md"), "gone").expect("stale");

    let changes = plan(temp.path()).expect("plan");

    let summary: Vec<(Action, PathBuf)> = changes
      .iter()
      .map(|change| (change.action, change.path.clone()))
      .collect();
    assert_eq!(
      summary,
      [
        (Action::Update, dir.join("SKILL.md")),
        (Action::Remove, dir.join("references/retired.md")),
      ]
    );

    apply(&changes).expect("apply");
    assert_eq!(plan(temp.path()).expect("plan"), []);
    assert!(!dir.join("references/retired.md").exists());
  }

  /// Other skills in the same directory are the user's.
  #[test]
  fn leaves_other_skills_alone() {
    let temp = TempDir::new().expect("temp dir");
    let theirs = temp.path().join("deploy/SKILL.md");
    std::fs::create_dir_all(theirs.parent().expect("parent")).expect("create");
    std::fs::write(&theirs, "how to deploy").expect("write");

    apply(&plan(temp.path()).expect("plan")).expect("apply");

    assert_eq!(std::fs::read_to_string(&theirs).expect("kept"), "how to deploy");
  }

  #[test]
  fn every_skill_is_named_for_discovery() {
    for skill in SKILLS {
      let (name, contents) = skill.files[0];

      assert_eq!(name, "SKILL.md");
      assert!(
        contents.starts_with(&format!("---\nname: {}\ndescription: ", skill.name)),
        "{}",
        skill.name
      );
    }
  }

  /// A reference file `SKILL.md` does not name is never read.
  #[test]
  fn every_skill_points_at_its_reference_files() {
    for skill in SKILLS {
      let (_, index) = skill.files[0];

      for (name, _) in &skill.files[1..] {
        assert!(
          index.contains(&format!("`{name}`")),
          "{}/SKILL.md does not name {name}",
          skill.name
        );
      }
    }
  }

  fn manifest_text() -> String {
    manifest_skill()
      .files
      .iter()
      .map(|(_, contents)| *contents)
      .collect()
  }

  /// Every field, with no `..Default::default()`: adding one to the manifest
  /// fails to compile here until the docs describe it too.
  fn every_key() -> Manifest {
    Manifest {
      claude: ClaudeConfig {
        home: Some("~/home".to_string()),
        seed_from_keychain: true,
        shared: vec!["agents".to_string()],
      },
      container: ContainerConfig {
        cpus: 4,
        env: vec!["TOKEN".to_string()],
        memory: Memory::gibibytes(8),
        setup: vec!["true".to_string()],
      },
      host: HostConfig {
        clipboard: true,
        concurrency: 8,
        ports: vec![7001],
        commands: [(
          "test".to_string(),
          HostCommand {
            arguments: true,
            argv: vec!["true".to_string()],
            deny: vec!["-Z".to_string()],
            tty: true,
          },
        )]
        .into(),
      },
      image: ImageConfig {
        packages: vec!["jq".to_string()],
        run: vec!["true".to_string()],
        run_as_root: vec!["true".to_string()],
      },
      paths: vec![PathEntry {
        local: false,
        readonly: true,
        source: "~/src".to_string(),
        target: Some("/src".to_string()),
      }],
      project: ProjectConfig {
        image: "image".to_string(),
        name: Some("name".to_string()),
      },
      workspace: WorkspaceConfig {
        roots: vec!["~/workspace".to_string()],
      },
    }
  }

  #[test]
  fn the_manifest_docs_describe_every_key() {
    let docs = manifest_text();
    let toml::Value::Table(tables) = toml::Value::try_from(every_key()).expect("manifest should serialize") else {
      panic!("a manifest is a table");
    };

    for (table, value) in &tables {
      let (heading, keys) = match (table.as_str(), value) {
        ("paths", toml::Value::Array(entries)) => ("[[paths]]".to_string(), entries[0].clone()),
        (_, value) => (format!("[{table}]"), value.clone()),
      };
      assert!(docs.contains(&heading), "the docs have no {heading} section");

      let toml::Value::Table(mut keys) = keys else {
        panic!("{heading} is a table");
      };
      if let Some(toml::Value::Table(commands)) = keys.remove("commands") {
        assert!(docs.contains("[host.commands.<name>]"));
        let toml::Value::Table(command) = &commands["test"] else {
          panic!("a host command is a table");
        };
        keys.extend(command.clone());
      }

      for key in keys.keys() {
        assert!(
          docs.contains(&format!("- `{key}`")),
          "the docs do not describe `{key}` of {heading}"
        );
      }
    }
  }

  /// Docs teaching a key the manifest refuses are worse than none.
  #[test]
  fn the_manifest_docs_examples_parse() {
    let docs = manifest_text();
    let examples: Vec<&str> = docs
      .split("``` toml\n")
      .skip(1)
      .map(|block| block.split("```").next().expect("a block should close"))
      .collect();

    assert!(!examples.is_empty());
    for example in examples {
      toml::from_str::<Manifest>(example).unwrap_or_else(|error| panic!("{example}\n{error}"));
    }
  }
}
