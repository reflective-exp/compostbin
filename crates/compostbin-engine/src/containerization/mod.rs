mod build;
mod cache;
mod control;
mod files;
mod nat;
mod oci;
mod progress;
mod provision;
mod snapshots;
mod spec;
mod stdio;
mod store;
mod terminal;
mod unpacked;

use crate::builder::Builder;
use crate::engine::{Engine, Listener};
use crate::error::EngineError;
use crate::model::{BuildPlan, ExecSpec, RunSpec};
use containerization_framework::containerization::container::container_manager::CreateWithRootfsOptions;
use containerization_framework::containerization::container::{ContainerManager, LinuxContainer};
use containerization_framework::containerization::image::{Image, ImageStore};
use containerization_framework::containerization::process::{LinuxProcess, LinuxProcessConfiguration};
use containerization_framework::containerization::vm::{Kernel, SystemPlatform, VsockListener};
use containerization_framework::containerization_error::Code;
use containerization_framework::containerization_oci::image::{ImageConfig, Platform};
use std::collections::HashMap;
use std::error::Error;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use stdio::{Stdio, attached, is_tty};
use unpacked::Unpacked;

pub use control::served;
pub use store::{Store, StoreError};

pub fn control_socket(runtime_dir: &Path, name: &str) -> PathBuf {
  runtime_dir.join(name).join(control::CONTROL_SOCKET)
}

/// Distinct from joiners' `attach-<n>` ids.
const OWNER_ATTACH: &str = "attach-owner";

/// Exit code for an attach that never ran; outside the guest's range.
const FAILED: i32 = -1;

type Report = dyn Fn(&dyn Error) + Send + Sync;

fn note(message: &str) {
  eprintln!("{message}");
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Booted {
  /// Must be kept alive for as long as the container runs.
  _manager: ContainerManager,
  container: LinuxContainer,
  /// Base for every exec; a bare exec would ignore the image's user and environment.
  seed: LinuxProcessConfiguration,
}

#[derive(Default)]
struct Running {
  containers: Mutex<HashMap<String, Arc<Booted>>>,
  processes: Mutex<HashMap<String, Arc<LinuxProcess>>>,
}

impl Running {
  fn booted(&self, name: &str) -> Option<Arc<Booted>> {
    lock(&self.containers).get(name).cloned()
  }

  fn attach(&self, name: &str, id: &str, request: &control::Request, stdio: Stdio) -> Result<i32, EngineError> {
    let booted = self
      .booted(name)
      .ok_or_else(|| EngineError::unavailable(format!("attach to {name}"), "this process is not running it"))?;
    let process = Arc::new(
      booted
        .container
        .exec(id, configuration(&booted.seed, request, stdio))?,
    );

    lock(&self.processes).insert(id.to_string(), Arc::clone(&process));

    let ran = (|| {
      process.start()?;

      if stdio.has_terminal() {
        let _ = self.resize(id, stdio.terminal);
      }

      process.wait(None)
    })();

    lock(&self.processes).remove(id);
    let _ = process.delete();

    Ok(ran?.exit_code)
  }

  /// A no-op once the process has gone.
  fn resize(&self, id: &str, terminal: RawFd) -> Result<(), EngineError> {
    let Some(process) = lock(&self.processes).get(id).cloned() else {
      return Ok(());
    };

    let size = terminal::size(terminal).map_err(|error| EngineError::failed(format!("resize {id}"), error))?;

    Ok(process.resize(size)?)
  }
}

fn configuration(
  seed: &LinuxProcessConfiguration,
  request: &control::Request,
  stdio: Stdio,
) -> LinuxProcessConfiguration {
  let mut process = seed.clone();

  process.arguments = request.arguments.clone();
  // Appended so the caller's variables override the image's.
  process
    .environment_variables
    .extend(request.environment.iter().cloned());
  process.working_directory = request.working_directory.clone();

  if let Some(user) = &request.user {
    process.user = build::user(Some(user));
  }

  if stdio.has_terminal() {
    // Not `setTerminalIO`, which would write output to the input terminal and lose a stdout redirection.
    process.terminal = true;
    process.stdin = attached(stdio.terminal);
    process.stdout = attached(stdio.stdout);

    if !process
      .environment_variables
      .iter()
      .any(|variable| variable.starts_with("TERM="))
    {
      process.environment_variables.push("TERM=xterm".to_string());
    }
  } else {
    process.stdin = attached(stdio.stdin);
    process.stdout = attached(stdio.stdout);
    process.stderr = attached(stdio.stderr);
  }

  process
}

/// Keeps the default `PATH` when the image declares none.
fn seed(config: &ImageConfig) -> Result<LinuxProcessConfiguration, containerization_framework::Error> {
  let mut process = LinuxProcessConfiguration::from_image_config(config)?;

  if !process
    .environment_variables
    .iter()
    .any(|variable| variable.starts_with("PATH="))
  {
    process
      .environment_variables
      .extend(LinuxProcessConfiguration::default().environment_variables);
  }

  Ok(process)
}

/// Never pulls.
fn built(images: &ImageStore, reference: &str) -> Result<Image, EngineError> {
  images.get(reference, false).map_err(|error| {
    if error.is_code(Code::NotFound) {
      EngineError::unavailable(
        format!("find {reference}"),
        "it has not been built; run `compostbin build`",
      )
    } else {
      error.into()
    }
  })
}

pub struct FrameworkEngine {
  attach_failed: Arc<Report>,
  running: Arc<Running>,
  runtime_dir: PathBuf,
  store: Store,
}

impl FrameworkEngine {
  pub fn new(runtime_dir: impl Into<PathBuf>, store: Store) -> Self {
    Self {
      attach_failed: Arc::new(|_| {}),
      running: Arc::default(),
      runtime_dir: runtime_dir.into(),
      store,
    }
  }

  /// `report` receives errors from joiners' attaches, which have no caller to return to.
  pub fn reporting(self, report: impl Fn(&dyn Error) + Send + Sync + 'static) -> Self {
    Self {
      attach_failed: Arc::new(report),
      ..self
    }
  }

  /// Clears the container's directory first, since nothing cleaned up after a previous run.
  fn boot(&self, spec: &RunSpec) -> Result<Booted, EngineError> {
    let directory = self.store.container_dir(&spec.name);
    let _ = std::fs::remove_dir_all(&directory);

    let images = self.store.images()?;
    let kernel = Kernel::new(self.store.kernel(), SystemPlatform::LINUX_ARM);
    let mut manager = ContainerManager::new(
      &kernel,
      &provision::initfs_mount(&self.store),
      &images,
      Default::default(),
    )?;
    let image = images.get(&spec.image, true)?;
    let platform = Platform::current()?;
    let seed = seed(&image.config(&platform)?.config.unwrap_or_default())?;

    std::fs::create_dir_all(&directory).map_err(|error| EngineError::failed(format!("boot {}", spec.name), error))?;

    let rootfs = Unpacked::at(self.store.unpacked()).rootfs(&image, &platform, &directory.join("rootfs.ext4"))?;

    // The manager has no `Network`; interfaces are configured by `spec` on Virtualization's NAT.
    let options = CreateWithRootfsOptions {
      networking: false,
      vm: spec::vm(spec.resources),
      ..Default::default()
    };
    let container = manager.create_with_rootfs(&spec.name, &image, rootfs, options, spec::configure(spec))?;

    container.create()?;
    container.start()?;

    Ok(Booted {
      _manager: manager,
      container,
      seed,
    })
  }

  fn serve_control_socket(&self, name: String) -> Result<(), EngineError> {
    let path = control_socket(&self.runtime_dir, &name);
    let listener = control::bind(&path).map_err(|error| EngineError::failed("bind the control socket", error))?;
    let attach_failed = Arc::clone(&self.attach_failed);
    let running = Arc::clone(&self.running);

    std::thread::spawn(move || {
      control::serve(
        &listener,
        |request, stdio, id| match running.attach(&name, id, request, *stdio) {
          Ok(code) => code,
          Err(error) => {
            attach_failed(&error);
            FAILED
          }
        },
        |id, terminal| {
          let _ = running.resize(id, terminal);
        },
        |error| attach_failed(&error),
      );
    });

    Ok(())
  }
}

impl Engine for FrameworkEngine {
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    let descriptor = std::io::stdin().as_raw_fd();
    // Both stdin and stdout must be terminals, so redirected output gets no escape codes.
    let has_terminal = spec.tty && is_tty(descriptor) && is_tty(libc::STDOUT_FILENO);

    let _raw = if has_terminal {
      Some(terminal::Raw::acquire(descriptor).map_err(|error| EngineError::failed("raw mode", error))?)
    } else {
      None
    };

    let request = control::Request {
      arguments: spec.arguments.clone(),
      environment: spec::environment(&spec.env),
      user: spec.user.clone(),
      working_directory: spec::working_directory(spec.workdir.as_deref()),
    };

    let stdio = if has_terminal {
      terminal::watch_for_resize();
      Stdio::terminal(descriptor, libc::STDOUT_FILENO)
    } else {
      Stdio::inherit(spec.interactive)
    };

    if self.running.booted(&spec.name).is_some() {
      let attach = || {
        self
          .running
          .attach(&spec.name, OWNER_ATTACH, &request, stdio)
      };

      return if has_terminal {
        terminal::while_resizing(
          &terminal::resized,
          || {
            let _ = self.running.resize(OWNER_ATTACH, descriptor);
          },
          attach,
        )
      } else {
        attach()
      };
    }

    control::request(
      &control_socket(&self.runtime_dir, &spec.name),
      &request,
      &stdio,
      &terminal::resized,
    )
    .map_err(|error| EngineError::failed("attach", error))
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    self.store.references()
  }

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError> {
    let booted = self.boot(spec)?;

    lock(&self.running.containers).insert(spec.name.clone(), Arc::new(booted));

    self.serve_control_socket(spec.name.clone())
  }

  fn listen(&self, name: &str, port: u32) -> Result<Box<dyn Listener>, EngineError> {
    let booted = self
      .running
      .booted(name)
      .ok_or_else(|| EngineError::unavailable(format!("listen for {name}"), "this process is not running it"))?;
    let listener = booted
      .container
      .with_virtual_machine_instance(|instance| instance.listen(port))?;

    Ok(Box::new(listener))
  }

  fn is_running(&self, name: &str) -> bool {
    // Not `Running`, which only knows this process's containers.
    control::served(&control_socket(&self.runtime_dir, name))
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    let image = built(&self.store.images()?, image)?;

    Ok(Unpacked::at(self.store.unpacked()).holds(&image))
  }

  fn missing_content(&self, image: &str) -> Result<Option<String>, EngineError> {
    let (content, images) = self.store.content()?;

    for digest in built(&images, image)?.referenced_digests()? {
      if content.get(&digest)?.is_none() {
        return Ok(Some(format!("sha256:{digest}")));
      }
    }

    Ok(None)
  }

  fn version(&self) -> String {
    format!(
      "Containerization {}, kernel {}",
      store::initfs_version(),
      store::kernel_version()
    )
  }
}

impl Listener for VsockListener {
  fn accept(&self) -> Option<OwnedFd> {
    self.into_iter().next()
  }

  fn finish(&self) {
    let _ = VsockListener::finish(self);
  }
}

pub struct FrameworkBuilder {
  store: Store,
}

impl FrameworkBuilder {
  pub fn new(store: Store) -> Self {
    Self { store }
  }
}

impl Builder for FrameworkBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    let keys = cache::keys(plan)?;
    let name = spec::builder_name().map_err(|error| EngineError::failed(format!("build {}", plan.tag), error))?;

    build::build(&self.store, plan, keys, &name)
  }

  fn provision(&self) -> Result<(), EngineError> {
    provision::provision(&self.store)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn request() -> control::Request {
    control::Request {
      arguments: vec!["bash".to_string()],
      environment: vec!["IS_SANDBOX=1".to_string()],
      user: None,
      working_directory: "/workspace".to_string(),
    }
  }

  fn image_seed() -> LinuxProcessConfiguration {
    let config = ImageConfig {
      user: Some("claude".to_string()),
      env: Some(vec!["PATH=/usr/bin".to_string(), "LANG=C".to_string()]),
      working_dir: Some("/workspace".to_string()),
      ..ImageConfig::default()
    };

    seed(&config).expect("a seed")
  }

  #[test]
  fn seeds_a_process_from_its_images_user_and_environment() {
    let seed = image_seed();

    assert_eq!(seed.user.username, "claude");
    assert_eq!(seed.environment_variables, ["PATH=/usr/bin", "LANG=C"]);
    assert_eq!(seed.working_directory, "/workspace");
  }

  #[test]
  fn keeps_the_default_path_for_an_image_that_declares_none() {
    let seed = seed(&ImageConfig::default()).expect("a seed");

    assert_eq!(
      seed.environment_variables,
      [format!("PATH={}", LinuxProcessConfiguration::DEFAULT_PATH)]
    );
    assert_eq!(seed.user.username, "");
    assert_eq!(seed.working_directory, "/");
  }

  #[test]
  fn runs_a_request_as_the_images_user_unless_it_names_one() {
    let mut request = request();

    assert_eq!(
      configuration(&image_seed(), &request, Stdio::nothing())
        .user
        .username,
      "claude"
    );

    request.user = Some("root".to_string());

    assert_eq!(
      configuration(&image_seed(), &request, Stdio::nothing())
        .user
        .username,
      "root"
    );
  }

  #[test]
  fn gives_a_terminal_term_and_no_separate_stderr() {
    let process = configuration(&image_seed(), &request(), Stdio::terminal(7, 8));

    assert!(process.terminal);
    assert_eq!(
      (process.stdin, process.stdout, process.stderr),
      (Some(7), Some(8), None)
    );
    assert_eq!(
      process.environment_variables,
      ["PATH=/usr/bin", "LANG=C", "IS_SANDBOX=1", "TERM=xterm"]
    );
  }

  #[test]
  fn leaves_term_alone_when_the_caller_sets_it() {
    let mut request = request();
    request.environment.push("TERM=screen".to_string());

    let process = configuration(&image_seed(), &request, Stdio::terminal(7, 8));

    assert!(
      !process
        .environment_variables
        .contains(&"TERM=xterm".to_string())
    );
  }

  #[test]
  fn hands_a_caller_without_a_terminal_its_own_streams() {
    let process = configuration(&image_seed(), &request(), Stdio::inherit(false));

    assert!(!process.terminal);
    assert_eq!(
      (process.stdin, process.stdout, process.stderr),
      (None, Some(libc::STDOUT_FILENO), Some(libc::STDERR_FILENO))
    );
  }

  #[test]
  fn tells_the_caller_to_build_an_image_the_store_lacks() {
    let root = tempfile::tempdir().expect("a temp dir");
    let engine = FrameworkEngine::new(root.path().join("sessions"), Store::at(root.path().join("images")));

    for error in [
      engine
        .missing_content("compostbin/absent")
        .expect_err("nothing is built"),
      engine
        .is_unpacked("compostbin/absent")
        .expect_err("nothing is built"),
    ] {
      assert_eq!(
        error.to_string(),
        "cannot find compostbin/absent: it has not been built; run `compostbin build`"
      );
    }
  }
}
