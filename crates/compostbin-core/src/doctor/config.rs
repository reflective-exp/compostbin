use super::{Check, Status, check, listed};
use crate::error::ManifestError;
use crate::manifest::{MANIFEST_RELATIVE_PATH, TomlFile, USER_CONFIG_PATH, local_path, profile_path};
use crate::workspace::paths::PathResolver;
use std::fmt;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

const CLAUDE: &str = "claude";
const KEYCHAIN: &str = "keychain";

/// `[claude]` keys a manifest once took, each now the user config's `[claude] keychain`.
const MOVED_TO_USER_CONFIG: &[&str] = &[KEYCHAIN, "seed_from_keychain"];

pub struct Retired {
  path: PathBuf,
  key: &'static str,
  value: Value,
}

impl fmt::Display for Retired {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(formatter, "[{CLAUDE}] {} in {}", self.key, self.path.display())
  }
}

/// In precedence order: the first setting found is the one in effect.
pub fn manifest_files(profile: Option<&str>, resolver: &PathResolver, project_dir: &Path) -> Vec<PathBuf> {
  match profile {
    Some(name) => vec![resolver.resolve(&profile_path(name))],
    None => {
      let shared = project_dir.join(MANIFEST_RELATIVE_PATH);
      vec![local_path(&shared), shared]
    }
  }
}

pub fn retired(files: &[PathBuf]) -> Result<Vec<Retired>, ManifestError> {
  let mut found = Vec::new();

  for path in files {
    let Some(table) = Table::load_if_present(path)? else {
      continue;
    };
    let Some(Value::Table(claude)) = table.get(CLAUDE) else {
      continue;
    };

    for &key in MOVED_TO_USER_CONFIG {
      if let Some(value) = claude.get(key) {
        found.push(Retired {
          path: path.clone(),
          key,
          value: value.clone(),
        });
      }
    }
  }

  Ok(found)
}

pub fn configuration(retired: &[Retired], fixed: bool) -> Check {
  let items = retired.iter().map(ToString::to_string).collect();

  match (retired.is_empty(), fixed) {
    (true, _) => check("configuration", Status::Ok, "no retired keys"),
    (false, true) => listed(
      "configuration",
      Status::Ok,
      format!("moved to [{CLAUDE}] {KEYCHAIN} in {USER_CONFIG_PATH}"),
      items,
    ),
    (false, false) => listed(
      "configuration",
      Status::Fail,
      format!("retired; `compostbin doctor --fix` moves them to [{CLAUDE}] {KEYCHAIN} in {USER_CONFIG_PATH}"),
      items,
    ),
  }
}

/// The user config takes the setting that was in effect, unless it already sets its own.
pub fn fix(retired: &[Retired], user_config: &Path) -> Result<(), ManifestError> {
  if retired.iter().find_map(|found| found.value.as_bool()) == Some(true) {
    let mut config = Table::load_if_present(user_config)?.unwrap_or_default();

    let unset = match config
      .entry(CLAUDE)
      .or_insert_with(|| Value::Table(Table::new()))
    {
      Value::Table(claude) if !claude.contains_key(KEYCHAIN) => {
        claude.insert(KEYCHAIN.to_string(), Value::Boolean(true));
        true
      }
      _ => false,
    };

    if unset {
      config.save(user_config)?;
    }
  }

  let mut paths: Vec<&Path> = retired.iter().map(|found| found.path.as_path()).collect();
  paths.dedup();

  for path in paths {
    let mut table = Table::load_if_present(path)?.unwrap_or_default();

    if let Some(Value::Table(claude)) = table.get_mut(CLAUDE) {
      for key in MOVED_TO_USER_CONFIG {
        claude.remove(*key);
      }

      if claude.is_empty() {
        table.remove(CLAUDE);
      }
    }

    table.save(path)?;
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  struct Files {
    _temp: TempDir,
    shared: PathBuf,
    local: PathBuf,
    user: PathBuf,
  }

  impl Files {
    fn new(shared: &str, local: Option<&str>, user: Option<&str>) -> Self {
      let temp = TempDir::new().expect("temp dir");
      let shared_path = temp.path().join(MANIFEST_RELATIVE_PATH);
      let files = Self {
        local: local_path(&shared_path),
        shared: shared_path,
        user: temp.path().join("config.toml"),
        _temp: temp,
      };

      std::fs::create_dir_all(files.shared.parent().expect("manifest dir")).expect("create manifest dir");
      std::fs::write(&files.shared, shared).expect("write manifest");

      if let Some(local) = local {
        std::fs::write(&files.local, local).expect("write local manifest");
      }

      if let Some(user) = user {
        std::fs::write(&files.user, user).expect("write user config");
      }

      files
    }

    fn retired(&self) -> Vec<Retired> {
      retired(&[self.local.clone(), self.shared.clone()]).expect("files should parse")
    }

    fn fix(&self) {
      fix(&self.retired(), &self.user).expect("fix should succeed");
    }

    fn read(path: &Path) -> Table {
      Table::load_if_present(path)
        .expect("file should parse")
        .expect("file should exist")
    }
  }

  #[test]
  fn lists_retired_keys() {
    let files = Files::new(
      "[claude]\nkeychain = true\n",
      Some("[claude]\nseed_from_keychain = true\n"),
      None,
    );

    let check = configuration(&files.retired(), false);

    assert_eq!(check.status, Status::Fail);
    assert_eq!(
      check.items,
      [
        format!("[claude] seed_from_keychain in {}", files.local.display()),
        format!("[claude] keychain in {}", files.shared.display()),
      ]
    );
  }

  #[test]
  fn passes_current_keys() {
    let files = Files::new("[claude]\nshared = [\"commands\"]\n", None, None);

    assert!(files.retired().is_empty());
    assert_eq!(configuration(&[], false).status, Status::Ok);
  }

  #[test]
  fn moves_keychain_to_user_config() {
    let files = Files::new(
      "[claude]\nkeychain = true\nshared = [\"commands\"]\n\n[project]\nname = \"cb\"\n",
      None,
      Some("[claude]\nshared = [\"agents\"]\n"),
    );

    files.fix();

    assert_eq!(
      Files::read(&files.shared).to_string(),
      "[claude]\nshared = [\"commands\"]\n\n[project]\nname = \"cb\"\n"
    );
    assert_eq!(
      Files::read(&files.user).to_string(),
      "[claude]\nkeychain = true\nshared = [\"agents\"]\n"
    );
    assert!(files.retired().is_empty());
  }

  #[test]
  fn drops_empty_claude_table() {
    let files = Files::new(
      "[claude]\nseed_from_keychain = true\n\n[project]\nname = \"cb\"\n",
      None,
      None,
    );

    files.fix();

    assert_eq!(Files::read(&files.shared).to_string(), "[project]\nname = \"cb\"\n");
    assert_eq!(Files::read(&files.user).to_string(), "[claude]\nkeychain = true\n");
  }

  #[test]
  fn local_manifest_wins() {
    let files = Files::new(
      "[claude]\nkeychain = true\n",
      Some("[claude]\nkeychain = false\n"),
      None,
    );

    files.fix();

    assert!(!files.user.exists(), "off is the default, so nothing needs writing");
    assert!(files.retired().is_empty());
  }

  #[test]
  fn user_config_wins() {
    let files = Files::new(
      "[claude]\nkeychain = true\n",
      None,
      Some("[claude]\nkeychain = false\n"),
    );

    files.fix();

    assert_eq!(Files::read(&files.user).to_string(), "[claude]\nkeychain = false\n");
  }
}
