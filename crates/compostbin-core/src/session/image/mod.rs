//! What the base image is.
//!
//! `base_plan` is its definition: the steps, in the order they run, with the names
//! its build log shows. `project_plan` is the same for whatever a manifest adds on
//! top. The builder boots the base, runs the steps in it, and writes the result
//! back as an image.

use crate::error::{At, ImageError};
use crate::manifest::Manifest;
use crate::session::Session;
use compostbin_engine::builder::Builder;
use compostbin_engine::model::{BuildPlan, BuildStep, Resources};
use std::path::PathBuf;

/// Where the image store lives. Under `.cache` because everything in it is
/// regenerable: the images by a build, the kernel and the init image by a
/// download.
pub const IMAGE_STORE: &str = "~/.cache/compostbin/images";
/// Where the guest scripts are staged for a build to read. Kept afterwards, so
/// a failed build can be poked at; under `.cache` because it is regenerable.
pub const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
/// Where a build's context appears inside the builder. Steps install out of it,
/// which is what a `COPY` was.
/// The engine's: the build cache matches on it.
pub use compostbin_engine::cache::CONTEXT_MOUNT;
/// The builder's size. Deliberately not the manifest's `[container]`: that is
/// what the session runs with, and a project asking for a bigger session has no
/// business resizing everyone's build. Sized past 2 GB, which is not enough to
/// install Claude Code.
pub const BUILD_RESOURCES: Resources = Resources {
  cpus: 4,
  memory_in_bytes: 8 << 30,
};
/// What the base image is built from. Registry-qualified: nothing downstream
/// resolves a bare `debian:stable-slim`.
pub const BASE_IMAGE: &str = "docker.io/library/debian:stable-slim";
/// The unprivileged user every session runs as, created by the base image.
pub const USER: &str = "claude";
/// Claude's home, and the directory the session's project is mounted under.
pub const HOME: &str = "/home/claude";
pub const WORKSPACE: &str = "/workspace";
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

/// Every script a build's context has to hold, since the steps install them by
/// name and a context missing one fails the build.
pub const GUEST_SCRIPTS: [(&str, &str); 3] = [
  (GUEST_CLIENT_NAME, GUEST_CLIENT),
  (GUEST_CLIPBOARD_NAME, GUEST_CLIPBOARD),
  (GUEST_PORTS_NAME, GUEST_PORTS),
];

/// Builds the base image, and then the project's own when the manifest adds
/// anything to it.
///
/// The base is shared by every project, so anything belonging to one project —
/// direnv, a language toolchain, a private CA — goes in the derived image rather
/// than growing the base for everyone.
pub fn build(session: &Session, builder: &impl Builder, cache: bool) -> Result<(), ImageError> {
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

/// The base image: debian, the tools a session needs, Claude Code, the guest
/// side of the host channel, and the user it all ends up running as.
pub fn base_plan(session: &Session) -> BuildPlan {
  let mut plan = BuildPlan::new(BASE_IMAGE, &session.manifest.project.image, BUILD_RESOURCES);

  plan.context = Some(context(session));
  // Keeps `.claude.json` — the account and the onboarding answers — inside the
  // mounted home. Without it Claude writes to `~/.claude.json`, which dies with
  // the container, and every restart asks for a fresh login.
  plan.environment = vec![format!("CLAUDE_CONFIG_DIR={HOME}/.claude")];
  // Nothing in a session needs root: the mounts are the user's own files, and a
  // container that installs packages at run time is one whose image is wrong.
  plan.user = Some(USER.to_string());
  plan.workdir = Some(PathBuf::from(WORKSPACE));
  plan.steps = vec![
    BuildStep::root(
      "packages",
      format!(
        "apt-get update \\\n && apt-get install --no-install-recommends --yes \\\n{} \\\n && rm -rf /var/lib/apt/lists/*",
        PACKAGES
          .map(|package| format!("      {package}"))
          .join(" \\\n")
      ),
    ),
    BuildStep::root(
      "claude code",
      "curl -fsSL https://claude.ai/install.sh | bash \\\n && mv /root/.local/share/claude/versions/* /usr/local/bin/claude",
    ),
    BuildStep::root(
      "guest scripts",
      format!(
        "install -m 755 {CONTEXT_MOUNT}/{GUEST_CLIENT_NAME} /usr/local/bin/{GUEST_CLIENT_NAME} \\\n \
         && install -m 755 {CONTEXT_MOUNT}/{GUEST_CLIPBOARD_NAME} /usr/local/bin/{GUEST_CLIPBOARD_NAME} \\\n \
         && install -m 755 {CONTEXT_MOUNT}/{GUEST_PORTS_NAME} /usr/local/bin/{GUEST_PORTS_NAME} \\\n \
         && for tool in {tools}; do \\\n      ln -s {GUEST_CLIPBOARD_NAME} \"/usr/local/bin/$tool\"; \\\n    done",
        tools = CLIPBOARD_TOOLS.join(" "),
      ),
    ),
    BuildStep::root(
      "the session's user",
      format!(
        // `/etc/claude-code` is where Claude's managed settings are mounted at
        // run time — the session's briefing. The mountpoint exists here because
        // the guest cannot create one under /etc, and stays root-owned because
        // nothing in a session may edit what it is told.
        //
        // The home and the workspace are mounted from the host too; created here,
        // owned by the user, so the first run has a mountpoint even before the
        // host directory exists and nothing under them is left owned by root.
        "mkdir -p /etc/claude-code \\\n \
         && useradd --create-home --shell /bin/bash {USER} \\\n \
         && mkdir -p {HOME}/.claude {WORKSPACE} \\\n \
         && chown -R {USER}:{USER} {HOME} {WORKSPACE}"
      ),
    ),
  ];

  plan
}

/// The plan for the project's own image, or `None` when the manifest adds
/// nothing and the session can run the base itself.
///
/// One root group then one user group, in that order: packages install as root
/// because apt needs to, `run_as_root` lines follow while the image is still
/// root, and `run` lines execute as the session's user, so a line appending to
/// `~/.bashrc` writes the file the session will read.
///
/// The image always ends as that user, whether or not anything ran as them: a
/// session must not run as root.
pub fn project_plan(session: &Session) -> Option<BuildPlan> {
  let manifest = &session.manifest;

  if manifest.image.is_empty() {
    return None;
  }

  let mut plan = BuildPlan::new(&manifest.project.image, session.image(), BUILD_RESOURCES);

  plan.user = Some(USER.to_string());
  plan.workdir = Some(PathBuf::from(HOME));

  if !manifest.image.packages.is_empty() {
    plan.steps.push(BuildStep::root(
      "packages",
      format!(
        "apt-get update \\\n && apt-get install --no-install-recommends --yes \\\n{} \\\n && rm -rf /var/lib/apt/lists/*",
        manifest
          .image
          .packages
          .iter()
          .map(|package| format!("      {package}"))
          .collect::<Vec<_>>()
          .join(" \\\n")
      ),
    ));
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

/// Whether a manifest adds anything to the base image.
pub fn adds_to_the_base(manifest: &Manifest) -> bool {
  !manifest.image.is_empty()
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
  use crate::manifest::Manifest;
  use crate::workspace::paths::PathResolver;
  use compostbin_engine::fake::RecordingBuilder;
  use tempfile::TempDir;

  fn session(home: &TempDir) -> Session {
    let base = home.path().canonicalize().expect("canonical temp");

    Session::new(
      Manifest::default(),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    )
  }

  /// The steps install each script by name, so a context missing one fails the
  /// build.
  #[test]
  fn writes_every_guest_script_into_the_build_context() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);

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
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);
    let plan = base_plan(&session);

    assert_eq!(plan.base, BASE_IMAGE);
    assert_eq!(plan.tag, "compostbin/base:latest");
    assert_eq!(plan.context, Some(context(&session)));
  }

  #[test]
  fn base_image_ends_as_an_unprivileged_user_in_the_workspace() {
    let home = TempDir::new().expect("temp dir");
    let plan = base_plan(&session(&home));

    assert_eq!(plan.user, Some(USER.to_string()), "a session must not run as root");
    assert_eq!(plan.workdir, Some(PathBuf::from(WORKSPACE)));
    assert_eq!(plan.environment, [format!("CLAUDE_CONFIG_DIR={HOME}/.claude")]);
  }

  #[test]
  fn base_image_installs_claude_code_and_the_guest_scripts() {
    let home = TempDir::new().expect("temp dir");
    let plan = base_plan(&session(&home));
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
    let home = TempDir::new().expect("temp dir");

    for step in base_plan(&session(&home)).steps {
      assert_eq!(step.user, None, "{} should run as root", step.name);
    }
  }

  /// The base is shared and byte-identical everywhere. A project asking for a
  /// bigger session must not resize the build.
  #[test]
  fn the_session_memory_does_not_reach_the_build() {
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest.container.cpus = 16;
    session.manifest.container.memory = crate::manifest::Memory::gibibytes(16);
    session.manifest.image.packages = vec!["direnv".to_string()];
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    for plan in builder.plans() {
      assert_eq!(plan.resources, BUILD_RESOURCES, "{plan:?}");
    }
  }

  #[test]
  fn adds_nothing_to_the_base_image_by_default() {
    let home = TempDir::new().expect("temp dir");

    assert_eq!(project_plan(&session(&home)), None);
    assert!(!adds_to_the_base(&Manifest::default()));
  }

  #[test]
  fn builds_a_project_image_from_manifest_additions() {
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest = toml::from_str("[image]\npackages = [\"direnv\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n")
      .expect("manifest should parse");
    let plan = project_plan(&session).expect("additions should produce a plan");

    assert_eq!(
      plan.base, "compostbin/base:latest",
      "the project image extends the base rather than repeating it"
    );
    assert_eq!(plan.tag, session.image());
    assert!(plan.steps[0].script.contains("      direnv"), "{:?}", plan.steps);
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
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest = toml::from_str(
      "[image]\npackages = [\"ca-certificates\"]\nrun_as_root = [\"cp /tmp/ca.crt /usr/local/share/ca-certificates/\", \"update-ca-certificates\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n",
    )
    .expect("manifest should parse");

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
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest =
      toml::from_str("[image]\nrun_as_root = [\"install -d /opt/vendor\"]\n").expect("manifest should parse");

    let plan = project_plan(&session).expect("additions should produce a plan");

    assert_eq!(
      plan.steps,
      [BuildStep::root("install -d /opt/vendor", "install -d /opt/vendor")]
    );
    assert_eq!(plan.user, Some(USER.to_string()), "apt left the image on root");
  }

  #[test]
  fn builds_the_project_image_after_the_base() {
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest.image.packages = vec!["direnv".to_string()];
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    let plans = builder.plans();
    assert_eq!(plans.len(), 2, "base, then project: {plans:?}");
    assert_eq!(plans[0].tag, "compostbin/base:latest");
    assert_eq!(plans[1].tag, session.image());
    assert_eq!(plans[1].base, plans[0].tag);
  }

  #[test]
  fn builds_only_the_base_when_the_manifest_adds_nothing() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);
    let builder = RecordingBuilder::new();

    build(&session, &builder, true).expect("build should succeed");

    assert_eq!(builder.plans().len(), 1, "the base only");
  }
}
