//! [`Engine`] and [`Builder`], backed by the `containerization-framework` crate.
//!
//! That crate boots a container and runs processes in it; everything around
//! that is here, because it is compostbin's rather than the framework's:
//!
//! - a container lives *in* the process that booted it, so whichever command
//!   creates one also serves the control socket through which later ones join
//!   ([`control`], [`terminal`]);
//! - nothing lists containers, so a container is up iff something answers on
//!   its control socket;
//! - where a guest sits on the NAT network ([`nat`]), what invalidates a build
//!   ([`cache`]), and what a builder container is called ([`spec`]) are all
//!   compostbin's to decide.

mod cache;
mod control;
mod nat;
mod spec;
mod terminal;

use crate::builder::Builder;
use crate::engine::Engine;
use crate::error::EngineError;
use crate::model::{BuildPlan, ExecSpec, RunSpec};
use containerization_framework::{self as framework, Session, Stdio};
use std::error::Error;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use containerization_framework::{Store, StoreError};
pub use control::served;

/// The store at `root`, booting the kernel and init image the framework pins.
///
/// Each is named for its version, so a framework upgrade that moves a pin finds
/// nothing at the new path and provisioning fetches it, rather than booting
/// what an older release left behind.
pub fn store(root: impl Into<PathBuf>) -> Store {
  let root = root.into();
  let kernel = root.join(format!("kernels/vmlinux-{}", framework::KERNEL_VERSION));
  let initfs = root.join(format!("initfs/vminit-{}.ext4", framework::INITFS_VERSION));

  Store::at(root, kernel, framework::INITFS_REFERENCE, initfs)
}

/// Where the container `name` answers attaches while it runs, given the
/// directory a `FrameworkEngine` keeps one runtime directory per container in.
pub fn control_socket(runtime_dir: &Path, name: &str) -> PathBuf {
  runtime_dir.join(name).join(control::CONTROL_SOCKET)
}

/// The owner's exec id; can't collide with a joiner's `attach-<n>`.
const OWNER_ATTACH: &str = "attach-owner";

/// What an attach that never ran reports. Outside the guest exit code range.
const FAILED: i32 = -1;

/// What `FrameworkEngine::reporting` is given.
type Report = dyn Fn(&dyn Error) + Send + Sync;

pub struct FrameworkEngine {
  /// Told about a joined client whose attach broke, since that leaves the rest
  /// of the session running and has no call to return to. Ignored by default.
  attach_failed: Arc<Report>,
  /// One directory per container, holding its control socket.
  runtime_dir: PathBuf,
  session: Session,
}

impl FrameworkEngine {
  pub fn new(runtime_dir: impl Into<PathBuf>, store: Store) -> Self {
    Self {
      attach_failed: Arc::new(|_| {}),
      runtime_dir: runtime_dir.into(),
      session: Session::new(store),
    }
  }

  /// Hands broken attaches to `report`, which decides what to say about them.
  pub fn reporting(self, report: impl Fn(&dyn Error) + Send + Sync + 'static) -> Self {
    Self {
      attach_failed: Arc::new(report),
      ..self
    }
  }

  /// Runs a guest process against the caller's stdio, as the owner of the VM.
  fn attach(
    session: &Session,
    name: &str,
    id: &str,
    request: &control::Request,
    stdio: Stdio,
  ) -> Result<i32, framework::Error> {
    let process = framework::model::LinuxProcessConfiguration {
      arguments: Some(request.arguments.clone()),
      environment_variables: request.environment.clone(),
      working_directory: Some(request.working_directory.clone()),
      user: request.user.clone().map(framework::model::User::named),
    };

    session.exec(name, id, &process, stdio)
  }

  /// Serves attaches from other callers on a detached thread, never joined:
  /// it ends with the process, as does the VM.
  ///
  /// A joiner's attach has no call to return an error to, so a failure comes
  /// back as the code reserved for one and is reported beside it.
  fn serve_control_socket(&self, name: String) -> Result<(), EngineError> {
    let path = control_socket(&self.runtime_dir, &name);
    let listener = control::bind(&path).map_err(|error| EngineError::failed("bind the control socket", error))?;
    let attach_failed = Arc::clone(&self.attach_failed);
    let session = Session::new(self.session.store().clone());

    std::thread::spawn(move || {
      control::serve(
        &listener,
        // The request arrives resolved against the client's environment.
        |request, stdio, id| match Self::attach(&session, &name, id, request, *stdio) {
          Ok(code) => code,
          Err(error) => {
            attach_failed(&error);
            FAILED
          }
        },
        |id, terminal| {
          let _ = session.resize(id, terminal);
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
    // A terminal is the caller's to give, and it gives one only when both the
    // streams it interacts through are terminals. A piped prompt or a
    // redirected `run > log` leaves the guest without one, rather than writing
    // a terminal's escapes into whatever the caller redirected to.
    let attached = spec.tty && framework::is_tty(descriptor) && framework::is_tty(libc::STDOUT_FILENO);

    // Raw while attached, restored on drop. See `terminal` for why.
    let _raw = if attached {
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

    let stdio = if attached {
      // Only a terminal has a window, so only then is there anything to watch.
      terminal::watch_for_resize();
      Stdio::terminal(descriptor, libc::STDOUT_FILENO)
    } else {
      Stdio::inherit(spec.interactive)
    };

    // The owner attaches directly; anyone else asks the owner to.
    if self.session.is_running(&spec.name) {
      // Duplicates, since the framework closes what it is given.
      let duplicated = stdio
        .try_clone()
        .map_err(|error| EngineError::failed("duplicate the caller's stdio", error))?;

      let attach = || Self::attach(&self.session, &spec.name, OWNER_ATTACH, &request, duplicated);

      // This process holds the terminal and the VM, so it resizes the guest
      // directly; a joiner has to ask over the control socket.
      let code = if attached {
        terminal::while_resizing(
          &terminal::resized,
          || {
            let _ = self.session.resize(OWNER_ATTACH, descriptor);
          },
          attach,
        )
      } else {
        attach()
      };

      return Ok(code?);
    }

    // Not duplicated: `SCM_RIGHTS` already copies each one, and the owner dups
    // again.
    control::request(
      &control_socket(&self.runtime_dir, &spec.name),
      &request,
      &stdio,
      &terminal::resized,
    )
    .map_err(|error| EngineError::failed("attach", error))
  }

  /// Read from the store's index; there is no daemon to ask.
  fn images(&self) -> Result<Vec<String>, EngineError> {
    self
      .session
      .images()
      .map_err(|error| EngineError::unavailable("read the image index", error))
  }

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError> {
    self.session.boot(&spec::boot(spec))?;

    self.serve_control_socket(spec.name.clone())
  }

  fn is_running(&self, name: &str) -> bool {
    // A socket file with nothing answering is a container that died with its
    // owner. Not `Session::is_running`, which answers for this process alone.
    control::served(&control_socket(&self.runtime_dir, name))
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    Ok(self.session.is_unpacked(image)?)
  }

  fn version(&self) -> String {
    Session::version()
  }
}

pub struct FrameworkBuilder {
  builder: framework::Builder,
}

impl FrameworkBuilder {
  pub fn new(store: Store) -> Self {
    Self {
      builder: framework::Builder::new(store),
    }
  }
}

impl Builder for FrameworkBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    let keys = cache::keys(plan)?;
    let name = spec::builder_name().map_err(|error| EngineError::failed(format!("build {}", plan.tag), error))?;

    Ok(self.builder.build(&spec::build(plan, keys, name))?)
  }

  fn provision(&self) -> Result<(), EngineError> {
    Ok(self.builder.provision()?)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_the_kernel_and_init_image_for_the_versions_the_framework_pins() {
    let store = store("/images");

    assert_eq!(
      store.kernel(),
      Path::new(&format!("/images/kernels/vmlinux-{}", framework::KERNEL_VERSION))
    );
    assert_eq!(
      store.initfs(),
      Path::new(&format!("/images/initfs/vminit-{}.ext4", framework::INITFS_VERSION))
    );
    assert_eq!(store.initfs_reference(), framework::INITFS_REFERENCE);
  }
}
