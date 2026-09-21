//! What the base image is.
//!
//! `base_plan` is its definition: the steps, in the order they run, with the names
//! its build log shows. `project_plan` is the same for whatever a manifest adds on
//! top. The builder boots the base, runs the steps in it, and writes the result
//! back as an image.

use crate::error::{At, ImageError};
use crate::session::briefing::MANAGED_SETTINGS_TARGET;
use crate::session::{CLAUDE_HOME_TARGET, Session};
use crate::workspace::WORKSPACE_TARGET;
use compostbin_engine::builder::Builder;
use compostbin_engine::model::{BuildMount, BuildPlan, BuildStep, Resources};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Where the image store lives. Under `.cache` because everything in it is
/// regenerable: the images by a build, the kernel and the init image by a
/// download.
pub const IMAGE_STORE: &str = "~/.cache/compostbin/images";
/// Where the guest scripts are staged for a build to read. Kept afterwards, so
/// a failed build can be poked at; under `.cache` because it is regenerable.
pub const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
/// Where the staged scripts appear inside the builder. A step that names it is
/// keyed on what it holds, so editing a guest script rebuilds that step alone.
pub const CONTEXT_MOUNT: &str = "/mnt/compostbin-context";
/// The builder's size. Not the manifest's `[container]`: a project asking for a
/// bigger session must not resize everyone's build. Above 2 GB, which is too
/// little to install Claude Code.
pub const BUILD_RESOURCES: Resources = Resources {
  cpus: 4,
  memory_in_bytes: 8 << 30,
};
/// What the base image is built from. Registry-qualified: nothing downstream
/// resolves a bare `debian:stable-slim`.
pub const BASE_IMAGE: &str = "docker.io/library/debian:stable-slim";

/// Marks an image as this tool's, so a store holding images from elsewhere
/// still says which are ours.
pub const BUILT_BY: (&str, &str) = ("dev.compostbin.built-by", env!("CARGO_PKG_VERSION"));
/// The unprivileged user every session runs as, created by the base image.
pub const USER: &str = "claude";
/// Claude's home.
pub const HOME: &str = "/home/claude";
/// The tools the base image installs. `socat` is the port relay's; the rest are
/// what a session needs to be usable.
pub const PACKAGES: [&str; 10] = [
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
/// One script under four names, each ahead of any real one on `PATH`, so a
/// project image that installs `xclip` still copies to the host's pasteboard.
pub const CLIPBOARD_TOOLS: [&str; 4] = ["pbcopy", "wl-copy", "xclip", "xsel"];
/// The guest-side client of the host-command channel. A shell script because the
/// container is Linux while compostbin itself is a macOS binary.
pub const GUEST_CLIENT: &str = include_str!("compostbin-host");
pub const GUEST_CLIENT_NAME: &str = "compostbin-host";
pub const GUEST_CLIPBOARD: &str = include_str!("compostbin-clipboard");
pub const GUEST_CLIPBOARD_NAME: &str = "compostbin-clipboard";
/// The guest half of host port forwarding, run as the container's own process
/// when the manifest declares ports.
pub const GUEST_PORTS: &str = include_str!("compostbin-ports");
pub const GUEST_PORTS_NAME: &str = "compostbin-ports";

/// Every script a build's context must hold: the steps install them by name.
pub const GUEST_SCRIPTS: [(&str, &str); 3] = [
  (GUEST_CLIENT_NAME, GUEST_CLIENT),
  (GUEST_CLIPBOARD_NAME, GUEST_CLIPBOARD),
  (GUEST_PORTS_NAME, GUEST_PORTS),
];

/// Builds the base image, and then the project's own when the manifest adds
/// anything to it.
///
/// The base is shared by every project, so anything belonging to one (a
/// language toolchain, a private CA) goes in the derived image.
///
/// Provisions the store first, so there is no separate setup step to remember.
pub fn build(session: &Session, builder: &impl Builder, cache: bool) -> Result<(), ImageError> {
  builder.provision()?;

  let context = context(session);
  std::fs::create_dir_all(&context).at(&context)?;

  for (name, contents) in GUEST_SCRIPTS {
    let path = context.join(name);
    std::fs::write(&path, contents).at(&path)?;
  }

  let mut base = base_plan(session);
  base.cache = cache;

  builder.build(&base)?;

  if let Some(mut plan) = project_plan(session) {
    plan.cache = cache;
    builder.build(&plan)?;
  }

  Ok(())
}

fn built_by() -> BTreeMap<String, String> {
  BTreeMap::from([(BUILT_BY.0.to_string(), BUILT_BY.1.to_string())])
}

/// The base image: debian, the tools a session needs, Claude Code, the guest
/// side of the host channel, and the user it all ends up running as.
pub fn base_plan(session: &Session) -> BuildPlan {
  let mut plan = BuildPlan::new(BASE_IMAGE, &session.manifest.project.image, BUILD_RESOURCES);

  plan.mounts = vec![BuildMount {
    destination: CONTEXT_MOUNT.to_string(),
    readonly: true,
    source: context(session),
  }];
  // Keeps `.claude.json` — the account and the onboarding answers — inside the
  // mounted home. Without it Claude writes to `~/.claude.json`, which dies with
  // the container, and every restart asks for a fresh login.
  plan.environment = vec![format!("CLAUDE_CONFIG_DIR={CLAUDE_HOME_TARGET}")];
  plan.labels = built_by();
  // Nothing in a session needs root: the mounts are the user's own files, and a
  // container that installs packages at run time is one whose image is wrong.
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
        // The managed-settings mountpoint (the briefing) is created here because
        // the guest cannot create it under /etc; root-owned because nothing in a
        // session may edit what it is told.
        //
        // The home and workspace mountpoints are created owned by the user, so
        // the first run has them before the host directories exist and nothing
        // under them is left owned by root.
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

/// The plan for the project's own image, or `None` when the manifest adds
/// nothing and the session can run the base itself.
///
/// Root steps, then user steps: packages (apt needs root), then `run_as_root`,
/// then `run` as the session's user, so a line appending to `~/.bashrc` writes
/// the file the session reads.
///
/// The image always ends as that user: a session must not run as root.
pub fn project_plan(session: &Session) -> Option<BuildPlan> {
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

/// The base image's build context, resolved against the host.
pub fn context(session: &Session) -> PathBuf {
  session.resolve(BUILD_CONTEXT).join("base")
}

/// Where the store lives, resolved against the host.
pub fn store(session: &Session) -> PathBuf {
  session.resolve(IMAGE_STORE)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::fixtures::session;
  use compostbin_engine::fake::{BuildCall, RecordingBuilder};

  /// The steps install each script by name, so a context missing one fails the
  /// build.
  #[test]
  fn writes_every_guest_script_into_the_build_context() {
    let (_home, session) = session("", "project");

    build(&session, &RecordingBuilder::new(), true).expect("build should succeed");

    for (name, contents) in GUEST_SCRIPTS {
      assert_eq!(
        std::fs::read_to_string(context(&session).join(name)).expect("the script should exist"),
        contents
      );
    }
  }

  #[test]
  fn builds_the_base_from_debian_into_the_manifests_image() {
    let (_home, session) = session("", "project");
    let plan = base_plan(&session);

    assert_eq!(plan.base, BASE_IMAGE);
    assert_eq!(plan.tag, "compostbin/base:latest");
    assert_eq!(
      plan.mounts,
      vec![BuildMount {
        destination: CONTEXT_MOUNT.to_string(),
        readonly: true,
        source: context(&session),
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
    let (_home, session) = session("", "project");
    let plan = base_plan(&session);

    assert_eq!(plan.user, Some(USER.to_string()), "a session must not run as root");
    assert_eq!(plan.workdir, Some(PathBuf::from(WORKSPACE_TARGET)));
    assert_eq!(plan.environment, [format!("CLAUDE_CONFIG_DIR={CLAUDE_HOME_TARGET}")]);
  }

  #[test]
  fn base_image_installs_claude_code_and_the_guest_scripts() {
    let (_home, session) = session("", "project");
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

  /// Every step of the base runs as root: the image only drops to the session's
  /// user at the end, and there is no user to drop to until `useradd` has run.
  #[test]
  fn every_base_step_runs_as_root() {
    let (_home, session) = session("", "project");

    for step in base_plan(&session).steps {
      assert_eq!(step.user, None, "{} should run as root", step.name);
    }
  }

  /// The base is shared and byte-identical everywhere. A project asking for a
  /// bigger session must not resize the build.
  #[test]
  fn the_session_memory_does_not_reach_the_build() {
    let (_home, mut session) = session("", "project");
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
    let (_home, session) = session("", "project");

    assert_eq!(project_plan(&session), None);
  }

  #[test]
  fn builds_a_project_image_from_manifest_additions() {
    let (_home, session) = session(
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

  /// The work only root can do: after the packages it may need, and before the
  /// image drops to the user the session runs as.
  #[test]
  fn runs_root_lines_between_the_packages_and_the_user_lines() {
    let (_home, session) = session(
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

  /// Root lines with no packages: the only steps, and still as root.
  #[test]
  fn runs_root_lines_alone() {
    let (_home, session) = session("[image]\nrun_as_root = [\"install -d /opt/vendor\"]\n", "project");

    let plan = project_plan(&session).expect("additions should produce a plan");

    assert_eq!(
      plan.steps,
      [BuildStep::root("install -d /opt/vendor", "install -d /opt/vendor")]
    );
    assert_eq!(plan.user, Some(USER.to_string()), "apt left the image on root");
  }

  #[test]
  fn builds_the_project_image_after_the_base() {
    let (_home, mut session) = session("", "project");
    session.manifest.image.packages = vec!["jq".to_string()];
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    let plans = builder.plans();
    assert_eq!(plans.len(), 2, "base, then project: {plans:?}");
    assert_eq!(plans[0].tag, "compostbin/base:latest");
    assert_eq!(plans[1].tag, session.image());
    assert_eq!(plans[1].base, plans[0].tag);
  }

  /// Nothing builds without a kernel and an init image in the store.
  #[test]
  fn provisions_the_store_before_the_first_build() {
    let (_home, session) = session("", "project");
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    assert_eq!(builder.calls().first(), Some(&BuildCall::Provision));
  }

  #[test]
  fn builds_only_the_base_when_the_manifest_adds_nothing() {
    let (_home, session) = session("", "project");
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    assert_eq!(builder.plans().len(), 1, "the base only");
  }
}
