use crate::error::{ImageError, PathError};
use crate::manifest::Manifest;
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::model::BuildSpec;
use std::path::PathBuf;

/// Where the embedded Dockerfile is written before building. Kept, so a failed
/// build can be reproduced by hand; under `.cache` because it is regenerable.
pub const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
/// The builder's size. A build-time constant, deliberately not the manifest's
/// `[container] memory`: that is what the session runs with, and letting it
/// reach the builder would make one project's runtime preference resize the
/// builder — and drop its layer cache — for every other project sharing the
/// base. Sized past the 2 GB default, which OOMs installing Claude Code.
pub const BUILD_MEMORY: &str = "8G";
pub const DOCKERFILE: &str = include_str!("Dockerfile");
pub const DOCKERFILE_NAME: &str = "Dockerfile";
/// The guest-side client, copied into the image by the Dockerfile.
pub const GUEST_CLIENT: &str = include_str!("compostbin-host");
pub const GUEST_CLIENT_NAME: &str = "compostbin-host";
/// The guest half of host port forwarding, run as the container's process when
/// ports are declared.
pub const GUEST_PORTS: &str = include_str!("compostbin-ports");
pub const GUEST_PORTS_NAME: &str = "compostbin-ports";

/// Builds the base image, and then the project's own image when the manifest
/// adds anything to it. The builder is started first, at `BUILD_MEMORY`:
/// installing Claude Code OOMs the builder's 2 GB default.
///
/// The base is shared by every project, so anything belonging to one project —
/// direnv, a language toolchain, a private CA — goes in the derived image rather
/// than growing the base for everyone.
pub fn build(session: &Session, engine: &impl Engine) -> Result<i32, ImageError> {
  let context = context(session);
  std::fs::create_dir_all(&context).map_err(|source| ImageError::Io(PathError::new(&context, source)))?;

  // The Dockerfile `COPY`s both scripts, so a context without one fails the build.
  for (name, contents) in [
    (DOCKERFILE_NAME, DOCKERFILE),
    (GUEST_CLIENT_NAME, GUEST_CLIENT),
    (GUEST_PORTS_NAME, GUEST_PORTS),
  ] {
    let path = context.join(name);
    std::fs::write(&path, contents).map_err(|source| ImageError::Io(PathError::new(&path, source)))?;
  }

  engine.start_builder(Some(BUILD_MEMORY))?;

  let code = engine.build(&BuildSpec {
    context,
    memory: Some(BUILD_MEMORY.to_string()),
    tag: session.manifest.project.image.clone(),
  })?;

  let Some(dockerfile) = project_dockerfile(&session.manifest) else {
    return Ok(code);
  };

  if code != 0 {
    return Ok(code);
  }

  let context = project_context(session);
  std::fs::create_dir_all(&context).map_err(|source| ImageError::Io(PathError::new(&context, source)))?;
  let path = context.join(DOCKERFILE_NAME);
  std::fs::write(&path, dockerfile).map_err(|source| ImageError::Io(PathError::new(&path, source)))?;

  Ok(engine.build(&BuildSpec {
    context,
    memory: Some(BUILD_MEMORY.to_string()),
    tag: session.image(),
  })?)
}

/// The Dockerfile for the project's own image, or `None` when the manifest adds
/// nothing and the session can run the base itself.
///
/// One root block then one user block, in that order: packages install as root
/// because apt needs to, `run_as_root` lines follow them while the image is
/// still root, and `run` lines execute as `claude`, so a line appending to
/// `~/.bashrc` writes the file the session will read.
///
/// The image always ends as `claude`, whether or not anything ran as the user:
/// a session must not run as root.
pub fn project_dockerfile(manifest: &Manifest) -> Option<String> {
  if manifest.image.is_empty() {
    return None;
  }

  let mut dockerfile = format!("FROM {}\n", manifest.project.image);

  if !manifest.image.packages.is_empty() || !manifest.image.run_as_root.is_empty() {
    dockerfile.push_str("\nUSER root\n");
  }

  if !manifest.image.packages.is_empty() {
    dockerfile.push_str("RUN apt-get update \\\n && apt-get install --no-install-recommends --yes \\\n");
    for package in &manifest.image.packages {
      dockerfile.push_str(&format!("      {package} \\\n"));
    }
    dockerfile.push_str(" && rm -rf /var/lib/apt/lists/*\n");
  }

  for line in &manifest.image.run_as_root {
    dockerfile.push_str(&format!("RUN {line}\n"));
  }

  dockerfile.push_str("\nUSER claude\n");
  for line in &manifest.image.run {
    dockerfile.push_str(&format!("RUN {line}\n"));
  }

  dockerfile.push_str("WORKDIR /home/claude\n");

  Some(dockerfile)
}

/// The base image's build context, resolved against the host.
pub fn context(session: &Session) -> PathBuf {
  session.resolve(BUILD_CONTEXT).join("base")
}

/// Beside the base's, named for the session so two projects building at once
/// cannot overwrite each other's Dockerfile.
pub fn project_context(session: &Session) -> PathBuf {
  session
    .resolve(BUILD_CONTEXT)
    .join(session.container_name())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::manifest::Manifest;
  use crate::workspace::paths::PathResolver;
  use apple_container::fake::RecordingEngine;
  use tempfile::TempDir;

  fn session(home: &TempDir) -> Session {
    let base = home.path().canonicalize().expect("canonical temp");

    Session::new(
      Manifest::default(),
      PathResolver::new(base.join("project"), &base),
      base.join("project"),
    )
  }

  #[test]
  fn writes_the_dockerfile_into_the_build_context() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);

    build(&session, &RecordingEngine::new()).expect("build should succeed");

    assert_eq!(
      std::fs::read_to_string(context(&session).join(DOCKERFILE_NAME)).expect("Dockerfile should exist"),
      DOCKERFILE
    );
  }

  /// The Dockerfile `COPY`s the client, so a context holding only the Dockerfile
  /// fails the build.
  #[test]
  fn writes_the_guest_client_into_the_build_context() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);

    build(&session, &RecordingEngine::new()).expect("build should succeed");

    let client = context(&session).join(GUEST_CLIENT_NAME);
    assert_eq!(
      std::fs::read_to_string(&client).expect("the client should exist"),
      GUEST_CLIENT
    );
  }

  #[test]
  fn starts_the_builder_before_building_the_manifest_image() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);
    let engine = RecordingEngine::new();

    build(&session, &engine).expect("build should succeed");

    assert_eq!(
      engine.calls(),
      [
        vec![
          "builder".to_string(),
          "start".to_string(),
          "--memory".to_string(),
          "8G".to_string()
        ],
        vec![
          "build".to_string(),
          "--memory".to_string(),
          "8G".to_string(),
          "--tag".to_string(),
          "compostbin/base:latest".to_string(),
          context(&session).display().to_string(),
        ],
      ]
    );
  }

  /// The base is shared and byte-identical everywhere. A project asking for a
  /// bigger session must not resize the builder, which would drop the layer
  /// cache every other project builds against.
  #[test]
  fn the_session_memory_does_not_reach_the_build() {
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest.container.memory = "16G".to_string();
    let engine = RecordingEngine::new();

    build(&session, &engine).expect("build should succeed");

    let calls = engine.calls();
    assert!(
      !calls.iter().flatten().any(|word| word == "16G"),
      "the runtime memory leaked into the build: {calls:?}"
    );
  }

  #[test]
  fn adds_nothing_to_the_base_image_by_default() {
    assert_eq!(project_dockerfile(&Manifest::default()), None);
  }

  #[test]
  fn builds_a_project_image_from_manifest_additions() {
    let manifest: Manifest = toml::from_str("[image]\npackages = [\"direnv\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n")
      .expect("manifest should parse");

    let dockerfile = project_dockerfile(&manifest).expect("additions should produce a Dockerfile");

    assert!(
      dockerfile.starts_with("FROM compostbin/base:latest\n"),
      "the project image extends the base rather than repeating it: {dockerfile}"
    );
    assert!(dockerfile.contains("      direnv \\\n"), "{dockerfile}");
    assert!(
      dockerfile.contains("USER claude\nRUN echo hook >> ~/.bashrc\n"),
      "a run line must execute as the user the session runs as: {dockerfile}"
    );
    assert!(
      dockerfile.trim_end().ends_with("WORKDIR /home/claude"),
      "the image must not be left sitting on root: {dockerfile}"
    );
    assert_eq!(
      dockerfile.matches("USER claude").count(),
      1,
      "the run block already restored the user; saying it again adds a layer for nothing: {dockerfile}"
    );
  }

  /// The work only root can do: after the packages it may need, and before the
  /// image drops to the user the session runs as.
  #[test]
  fn runs_root_lines_between_the_packages_and_the_user() {
    let manifest: Manifest = toml::from_str(
      "[image]\npackages = [\"ca-certificates\"]\nrun_as_root = [\"cp /tmp/ca.crt /usr/local/share/ca-certificates/\", \"update-ca-certificates\"]\nrun = [\"echo hook >> ~/.bashrc\"]\n",
    )
    .expect("manifest should parse");

    let dockerfile = project_dockerfile(&manifest).expect("additions should produce a Dockerfile");

    assert!(
      dockerfile.contains(
        " && rm -rf /var/lib/apt/lists/*\nRUN cp /tmp/ca.crt /usr/local/share/ca-certificates/\nRUN update-ca-certificates\n\nUSER claude\n"
      ),
      "root lines must follow the packages and precede the user switch: {dockerfile}"
    );
    assert_eq!(
      dockerfile.matches("USER root").count(),
      1,
      "packages and root lines share one root block: {dockerfile}"
    );
  }

  /// Root lines with no packages: nothing else has raised the user, so the root
  /// block has to open itself.
  #[test]
  fn raises_the_user_for_root_lines_alone() {
    let manifest: Manifest =
      toml::from_str("[image]\nrun_as_root = [\"install -d /opt/vendor\"]\n").expect("manifest should parse");

    let dockerfile = project_dockerfile(&manifest).expect("additions should produce a Dockerfile");

    assert_eq!(
      dockerfile,
      "FROM compostbin/base:latest\n\nUSER root\nRUN install -d /opt/vendor\n\nUSER claude\nWORKDIR /home/claude\n",
      "{dockerfile}"
    );
  }

  /// Packages with no run lines: nothing else restores the user, so the trailing
  /// `USER claude` is the one that has to.
  #[test]
  fn leaves_a_package_only_image_as_the_session_user() {
    let manifest: Manifest = toml::from_str("[image]\npackages = [\"direnv\"]\n").expect("manifest should parse");

    let dockerfile = project_dockerfile(&manifest).expect("additions should produce a Dockerfile");

    assert!(
      dockerfile
        .trim_end()
        .ends_with("USER claude\nWORKDIR /home/claude"),
      "apt left the image on root: {dockerfile}"
    );
  }

  #[test]
  fn builds_the_project_image_after_the_base() {
    let home = TempDir::new().expect("temp dir");
    let mut session = session(&home);
    session.manifest.image.packages = vec!["direnv".to_string()];
    let engine = RecordingEngine::new();

    build(&session, &engine).expect("build should succeed");

    let calls = engine.calls();
    assert_eq!(calls.len(), 3, "builder, base, project: {calls:?}");
    assert!(calls[1].contains(&"compostbin/base:latest".to_string()), "{calls:?}");
    assert!(calls[2].contains(&session.image()), "{calls:?}");
    assert_eq!(
      std::fs::read_to_string(project_context(&session).join(DOCKERFILE_NAME)).expect("Dockerfile should exist"),
      project_dockerfile(&session.manifest).expect("additions should produce a Dockerfile")
    );
  }

  #[test]
  fn builds_only_the_base_when_the_manifest_adds_nothing() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);
    let engine = RecordingEngine::new();

    build(&session, &engine).expect("build should succeed");

    assert_eq!(engine.calls().len(), 2, "builder and base only");
    assert!(!project_context(&session).exists());
  }

  #[test]
  fn base_image_ends_as_an_unprivileged_user() {
    let last_user = DOCKERFILE
      .lines()
      .filter(|line| line.starts_with("USER "))
      .last();

    assert_eq!(
      last_user,
      Some("USER claude"),
      "a session must not run as root: {DOCKERFILE}"
    );
    assert!(
      !DOCKERFILE.contains("/root/"),
      "nothing a session reads may live in root's home: {DOCKERFILE}"
    );
  }

  #[test]
  fn writes_port_relay_to_context() {
    let home = TempDir::new().expect("temp dir");
    let session = session(&home);

    build(&session, &RecordingEngine::new()).expect("build should succeed");

    assert_eq!(
      std::fs::read_to_string(context(&session).join(GUEST_PORTS_NAME)).expect("the relay should exist"),
      GUEST_PORTS
    );
  }

  #[test]
  fn installs_port_relay() {
    assert!(DOCKERFILE.contains("      socat \\\n"), "{DOCKERFILE}");
    assert!(
      DOCKERFILE.contains(&format!("COPY {GUEST_PORTS_NAME} /usr/local/bin/{GUEST_PORTS_NAME}")),
      "{DOCKERFILE}"
    );
  }

  #[test]
  fn dockerfile_installs_claude_code() {
    assert!(
      DOCKERFILE.contains("@anthropic-ai/claude-code"),
      "the base image is useless without Claude Code: {DOCKERFILE}"
    );
  }
}
