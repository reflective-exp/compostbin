use crate::error::{ImageError, PathError};
use crate::session::Session;
use apple_container::engine::Engine;
use apple_container::model::BuildSpec;
use std::path::PathBuf;

/// Where the embedded Dockerfile is written before building. Kept rather than
/// discarded so a failed build can be reproduced by hand, and under `.cache`
/// rather than `.local/state` because every byte of it is regenerable.
pub const BUILD_CONTEXT: &str = "~/.cache/compostbin/build";
pub const DOCKERFILE: &str = include_str!("image/Dockerfile");
pub const DOCKERFILE_NAME: &str = "Dockerfile";
/// The guest-side client, copied into the image by the Dockerfile.
pub const GUEST_CLIENT: &str = include_str!("image/compostbin-host");
pub const GUEST_CLIENT_NAME: &str = "compostbin-host";

/// Writes the embedded Dockerfile into a build context and builds the image the
/// manifest names. The builder is started first, at the manifest's memory size:
/// installing Claude Code OOMs the builder's 2 GB default.
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

  Ok(engine.build(&BuildSpec {
    context,
    memory: Some(memory),
    tag: session.manifest.project.image.clone(),
  })?)
}

/// The build context for this session, resolved against the host.
pub fn context(session: &Session) -> PathBuf {
  session.resolve(BUILD_CONTEXT)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::manifest::Manifest;
  use crate::paths::PathResolver;
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

  /// The Dockerfile `COPY`s the client, so the build fails outright if the
  /// context holds only the Dockerfile.
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
  fn dockerfile_installs_claude_code() {
    assert!(
      DOCKERFILE.contains("@anthropic-ai/claude-code"),
      "the base image is useless without Claude Code: {DOCKERFILE}"
    );
  }
}
