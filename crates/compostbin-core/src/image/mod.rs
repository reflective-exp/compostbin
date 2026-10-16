use crate::error::{At, ImageError};
use crate::session::briefing::MANAGED_SETTINGS_TARGET;
use crate::session::{CLAUDE_HOME_TARGET, Session};
use crate::workspace::WORKSPACE_TARGET;
use crate::workspace::paths::PathResolver;
use compostbin_engine::builder::Builder;
use compostbin_engine::model::{BuildMount, BuildPlan, BuildStep, Resources};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const IMAGE_STORE: &str = "~/.cache/compostbin/images";
const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
/// Steps that reference this path are cache-keyed on its contents.
const CONTEXT_MOUNT: &str = "/mnt/compostbin-context";
/// Independent of `[container]`; 2 GB is too little to install Claude Code.
const BUILD_RESOURCES: Resources = Resources {
  cpus: 4,
  memory_in_bytes: 8 << 30,
};
/// Must be registry-qualified; bare names are not resolved.
const BASE_IMAGE: &str = "docker.io/library/debian:stable-slim";

const BUILT_BY: (&str, &str) = ("dev.compostbin.built-by", env!("CARGO_PKG_VERSION"));
pub const USER: &str = "claude";
const HOME: &str = "/home/claude";
/// `socat` is required by the port relay.
const PACKAGES: [&str; 10] = [
  "ca-certificates",
  "curl",
  "git",
  "jq",
  "less",
  "openssh-client",
  "procps",
  "ripgrep",
  "socat",
  "unzip",
];
pub const CLIPBOARD_TOOLS: [&str; 4] = ["pbcopy", "wl-copy", "xclip", "xsel"];
const GUEST_CLIENT: &str = include_str!("compostbin-host");
const GUEST_CLIENT_NAME: &str = "compostbin-host";
const GUEST_CLIPBOARD: &str = include_str!("compostbin-clipboard");
const GUEST_CLIPBOARD_NAME: &str = "compostbin-clipboard";
const GUEST_PORTS: &str = include_str!("compostbin-ports");
pub(crate) const GUEST_PORTS_NAME: &str = "compostbin-ports";

const GUEST_SCRIPTS: [(&str, &str); 3] = [
  (GUEST_CLIENT_NAME, GUEST_CLIENT),
  (GUEST_CLIPBOARD_NAME, GUEST_CLIPBOARD),
  (GUEST_PORTS_NAME, GUEST_PORTS),
];

pub fn build(session: &Session, builder: &impl Builder, cache: bool) -> Result<(), ImageError> {
  builder.provision()?;

  for mut plan in plans(session)? {
    plan.cache = cache;
    builder.build(&plan)?;
  }

  Ok(())
}

/// In build order. Writes the build context first, since the plans' cache keys hash it.
pub fn plans(session: &Session) -> Result<Vec<BuildPlan>, ImageError> {
  let context = context(session.resolver());
  std::fs::create_dir_all(&context).at(&context)?;

  for (name, contents) in GUEST_SCRIPTS {
    let path = context.join(name);
    std::fs::write(&path, contents).at(&path)?;
  }

  Ok(
    std::iter::once(base_plan(session))
      .chain(project_plan(session))
      .collect(),
  )
}

fn built_by() -> BTreeMap<String, String> {
  BTreeMap::from([(BUILT_BY.0.to_string(), BUILT_BY.1.to_string())])
}

fn base_plan(session: &Session) -> BuildPlan {
  let mut plan = BuildPlan::new(BASE_IMAGE, &session.manifest.project.image, BUILD_RESOURCES);

  plan.mounts = vec![BuildMount {
    destination: CONTEXT_MOUNT.to_string(),
    source: context(session.resolver()),
  }];
  // Otherwise Claude writes `~/.claude.json` outside the mounted home and loses the login on restart.
  plan.environment = vec![format!("CLAUDE_CONFIG_DIR={CLAUDE_HOME_TARGET}")];
  plan.labels = built_by();
  plan.user = Some(USER.to_string());
  plan.workdir = Some(PathBuf::from(WORKSPACE_TARGET));
  plan.steps = vec![
    install_packages(PACKAGES),
    BuildStep::root(
      "claude code",
      "curl -fsSL https://claude.ai/install.sh | bash \\\n && mv /root/.local/share/claude/versions/* /usr/local/bin/claude",
    ),
    BuildStep::root(
      "guest scripts",
      format!(
        "{installs} \\\n && for tool in {tools}; do \\\n      ln -s {GUEST_CLIPBOARD_NAME} \"/usr/local/bin/$tool\"; \\\n    done",
        installs = GUEST_SCRIPTS
          .map(|(name, _)| format!("install -m 755 {CONTEXT_MOUNT}/{name} /usr/local/bin/{name}"))
          .join(" \\\n && "),
        tools = CLIPBOARD_TOOLS.join(" "),
      ),
    ),
    BuildStep::root(
      "the session's user",
      format!(
        // The guest cannot create mountpoints under /etc; the managed-settings one stays root-owned.
        "mkdir -p {MANAGED_SETTINGS_TARGET} \\\n \
         && useradd --create-home --shell /bin/bash {USER} \\\n \
         && mkdir -p {CLAUDE_HOME_TARGET} {WORKSPACE_TARGET} \\\n \
         && chown -R {USER}:{USER} {HOME} {WORKSPACE_TARGET}"
      ),
    ),
  ];

  plan
}

fn install_packages<'a>(packages: impl IntoIterator<Item = &'a str>) -> BuildStep {
  BuildStep::root(
    "packages",
    format!(
      "apt-get update \\\n && apt-get install --no-install-recommends --yes \\\n{} \\\n && rm -rf /var/lib/apt/lists/*",
      packages
        .into_iter()
        .map(|package| format!("      {package}"))
        .collect::<Vec<_>>()
        .join(" \\\n")
    ),
  )
}

fn project_plan(session: &Session) -> Option<BuildPlan> {
  let manifest = &session.manifest;

  if manifest.image.is_empty() {
    return None;
  }

  let mut plan = BuildPlan::new(&manifest.project.image, session.image(), BUILD_RESOURCES);

  plan.labels = built_by();
  plan.user = Some(USER.to_string());
  plan.workdir = Some(PathBuf::from(HOME));

  if !manifest.image.packages.is_empty() {
    plan
      .steps
      .push(install_packages(manifest.image.packages.iter().map(String::as_str)));
  }

  for line in &manifest.image.run_as_root {
    plan.steps.push(BuildStep::root(line, line.clone()));
  }

  for line in &manifest.image.run {
    plan
      .steps
      .push(BuildStep::as_user(line, USER, line.clone()));
  }

  Some(plan)
}

fn context(resolver: &PathResolver) -> PathBuf {
  resolver.resolve(BUILD_CONTEXT).join("base")
}

pub fn store(resolver: &PathResolver) -> PathBuf {
  resolver.resolve(IMAGE_STORE)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::session::fixtures::temp_session;
  use compostbin_engine::fake::{BuildCall, RecordingBuilder};

  #[test]
  fn writes_every_guest_script_into_the_build_context() {
    let (_home, session) = temp_session("", "project");

    build(&session, &RecordingBuilder::new(), true).expect("build should succeed");

    for (name, contents) in GUEST_SCRIPTS {
      assert_eq!(
        std::fs::read_to_string(context(session.resolver()).join(name)).expect("the script should exist"),
        contents
      );
    }
  }

  #[test]
  fn builds_the_base_from_debian_into_the_manifests_image() {
    let (_home, session) = temp_session("", "project");
    let plan = base_plan(&session);

    assert_eq!(plan.base, BASE_IMAGE);
    assert_eq!(plan.tag, "compostbin/base:latest");
    assert_eq!(
      plan.mounts,
      vec![BuildMount {
        destination: CONTEXT_MOUNT.to_string(),
        source: context(session.resolver()),
      }]
    );
    assert_eq!(
      plan.labels.get(BUILT_BY.0).map(String::as_str),
      Some(BUILT_BY.1),
      "the engine labels nothing on its own; this side says whose image it is"
    );
  }

  #[test]
  fn base_image_ends_as_an_unprivileged_user_in_the_workspace() {
    let (_home, session) = temp_session("", "project");
    let plan = base_plan(&session);

    assert_eq!(plan.user, Some(USER.to_string()), "a session must not run as root");
    assert_eq!(plan.workdir, Some(PathBuf::from(WORKSPACE_TARGET)));
    assert_eq!(plan.environment, [format!("CLAUDE_CONFIG_DIR={CLAUDE_HOME_TARGET}")]);
  }

  #[test]
  fn base_image_installs_claude_code_and_the_guest_scripts() {
    let (_home, session) = temp_session("", "project");
    let plan = base_plan(&session);
    let scripts = plan
      .steps
      .iter()
      .map(|step| step.script.clone())
      .collect::<String>();

    assert!(
      scripts.contains("https://claude.ai/install.sh"),
      "the base image is useless without Claude Code: {scripts}"
    );
    assert!(scripts.contains("      socat"), "the port relay needs socat: {scripts}");

    for (name, _) in GUEST_SCRIPTS {
      assert!(
        scripts.contains(&format!("{CONTEXT_MOUNT}/{name} /usr/local/bin/{name}")),
        "{name} is not installed: {scripts}"
      );
    }

    assert!(
      scripts.contains(&format!("for tool in {}", CLIPBOARD_TOOLS.join(" "))),
      "the clipboard tools are not linked: {scripts}"
    );
  }

  #[test]
  fn every_base_step_runs_as_root() {
    let (_home, session) = temp_session("", "project");

    for step in base_plan(&session).steps {
      assert_eq!(step.user, None, "{} should run as root", step.name);
    }
  }

  #[test]
  fn the_session_memory_does_not_reach_the_build() {
    let (_home, mut session) = temp_session("", "project");
    session.manifest.container.cpus = 16;
    session.manifest.container.memory = crate::manifest::Memory::gibibytes(16);
    session.manifest.image.packages = vec!["jq".to_string()];
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    for plan in builder.plans() {
      assert_eq!(plan.resources, BUILD_RESOURCES, "{plan:?}");
    }
  }

  #[test]
  fn adds_nothing_to_the_base_image_by_default() {
    let (_home, session) = temp_session("", "project");

    assert_eq!(project_plan(&session), None);
  }

  #[test]
  fn builds_a_project_image_from_manifest_additions() {
    let (_home, session) = temp_session(
      "[image]\npackages = [\"jq\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n",
      "project",
    );
    let plan = project_plan(&session).expect("additions should produce a plan");

    assert_eq!(
      plan.base, "compostbin/base:latest",
      "the project image extends the base rather than repeating it"
    );
    assert_eq!(plan.tag, session.image());
    assert_eq!(plan.labels.get(BUILT_BY.0).map(String::as_str), Some(BUILT_BY.1));
    assert!(plan.steps[0].script.contains("      jq"), "{:?}", plan.steps);
    assert_eq!(
      plan.steps[1],
      BuildStep::as_user("echo hook >> ~/.bashrc", USER, "echo hook >> ~/.bashrc"),
      "a run line must execute as the user the session runs as"
    );
    assert_eq!(
      plan.workdir,
      Some(PathBuf::from(HOME)),
      "the image must not be left sitting on root"
    );
    assert_eq!(plan.user, Some(USER.to_string()));
  }

  #[test]
  fn runs_root_lines_between_the_packages_and_the_user_lines() {
    let (_home, session) = temp_session(
      "[image]\npackages = [\"ca-certificates\"]\nrun_as_root = [\"cp /tmp/ca.crt /usr/local/share/ca-certificates/\", \"update-ca-certificates\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n",
      "project",
    );

    let steps = project_plan(&session)
      .expect("additions should produce a plan")
      .steps;

    assert!(steps[0].script.contains("ca-certificates"), "{steps:?}");
    assert_eq!(steps[1].user, None, "{steps:?}");
    assert_eq!(steps[2].script, "update-ca-certificates");
    assert_eq!(steps[2].user, None, "{steps:?}");
    assert_eq!(steps[3].user, Some(USER.to_string()), "{steps:?}");
    assert_eq!(steps.len(), 4, "{steps:?}");
  }

  #[test]
  fn runs_root_lines_alone() {
    let (_home, session) = temp_session("[image]\nrun_as_root = [\"install -d /opt/vendor\"]\n", "project");

    let plan = project_plan(&session).expect("additions should produce a plan");

    assert_eq!(
      plan.steps,
      [BuildStep::root("install -d /opt/vendor", "install -d /opt/vendor")]
    );
    assert_eq!(plan.user, Some(USER.to_string()), "apt left the image on root");
  }

  #[test]
  fn builds_the_project_image_after_the_base() {
    let (_home, mut session) = temp_session("", "project");
    session.manifest.image.packages = vec!["jq".to_string()];
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    let plans = builder.plans();
    assert_eq!(plans.len(), 2, "base, then project: {plans:?}");
    assert_eq!(plans[0].tag, "compostbin/base:latest");
    assert_eq!(plans[1].tag, session.image());
    assert_eq!(plans[1].base, plans[0].tag);
  }

  #[test]
  fn provisions_the_store_before_the_first_build() {
    let (_home, session) = temp_session("", "project");
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    assert_eq!(builder.calls().first(), Some(&BuildCall::Provision));
  }

  #[test]
  fn builds_only_the_base_when_the_manifest_adds_nothing() {
    let (_home, session) = temp_session("", "project");
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    assert_eq!(builder.plans().len(), 1, "the base only");
  }
}
