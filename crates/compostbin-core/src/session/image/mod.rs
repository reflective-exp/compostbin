use crate::error::{ImageError, PathError};
use crate::manifest::Manifest;
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::model::BuildSpec;
use std::path::PathBuf;

/// Where the embedded Dockerfile is written before building. Kept, so a failed
/// build can be reproduced by hand; under `.cache` because it is regenerable.
pub const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
pub const DOCKERFILE: &str = include_str!("Dockerfile");
pub const DOCKERFILE_NAME: &str = "Dockerfile";
/// The guest-side client, copied into the image by the Dockerfile.
pub const GUEST_CLIENT: &str = include_str!("compostbin-host");
pub const GUEST_CLIENT_NAME: &str = "compostbin-host";

/// Builds the base image, and then the project's own image when the manifest
/// adds anything to it. The builder is started first, at the manifest's memory
/// size: installing Claude Code OOMs the builder's 2 GB default.
///
/// The base is shared by every project, so anything belonging to one project —
/// direnv, a language toolchain, a private CA — goes in the derived image rather
/// than growing the base for everyone.
pub fn build(session: &Session, engine: &impl Engine) -> Result<i32, ImageError> {
  let context = context(session);
  std::fs::create_dir_all(&context).map_err(|source| ImageError::Io(PathError::new(&context, source)))?;

  // The Dockerfile `COPY`s the client, so a context without it fails the build.
  for (name, contents) in [(DOCKERFILE_NAME, DOCKERFILE), (GUEST_CLIENT_NAME, GUEST_CLIENT)] {
    let path = context.join(name);
    std::fs::write(&path, contents).map_err(|source| ImageError::Io(PathError::new(&path, source)))?;
  }

  let memory = session.manifest.container.memory.clone();
  engine.start_builder(Some(&memory))?;

  let code = engine.build(&BuildSpec {
    context,
    memory: Some(memory.clone()),
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
    memory: Some(memory),
    tag: session.image(),
  })?)
}

/// The Dockerfile for the project's own image, or `None` when the manifest adds
/// nothing and the session can run the base itself.
///
/// Packages install as root because apt needs to; `run` lines execute as
/// `claude`, so a line appending to `~/.bashrc` writes the file the session
/// will read.
pub fn project_dockerfile(manifest: &Manifest) -> Option<String> {
  if manifest.image.is_empty() {
    return None;
  }

  let mut dockerfile = format!("FROM {}\n", manifest.project.image);

  if !manifest.image.packages.is_empty() {
    dockerfile.push_str("\nUSER root\nRUN apt-get update \\\n && apt-get install --no-install-recommends --yes \\\n");
    for package in &manifest.image.packages {
      dockerfile.push_str(&format!("      {package} \\\n"));
    }
    dockerfile.push_str(" && rm -rf /var/lib/apt/lists/*\n");
  }

  if !manifest.image.run.is_empty() {
    dockerfile.push_str("\nUSER claude\n");
    for line in &manifest.image.run {
      dockerfile.push_str(&format!("RUN {line}\n"));
    }
  }

  dockerfile.push_str("\nUSER claude\nWORKDIR /home/claude\n");

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
  fn dockerfile_installs_claude_code() {
    assert!(
      DOCKERFILE.contains("@anthropic-ai/claude-code"),
      "the base image is useless without Claude Code: {DOCKERFILE}"
    );
  }
}
