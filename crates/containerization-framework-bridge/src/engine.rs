//! `Engine`, backed by Containerization.framework.
//!
//! A container lives *in* this process, not a daemon. So:
//!
//! - The VM lives exactly as long as this process; whichever command creates it
//!   also starts the control socket through which later ones join.
//! - Nothing lists containers. A container is up iff something answers on its
//!   control socket (one per container, under the runtime directory).

use crate::control::Stdio;
use crate::store::{INITFS_REFERENCE, Store};
use crate::{checked, control, ffi, spec, terminal};
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{ExecSpec, RunSpec};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGWINCH handler. One per process: a process attaches at most
/// one terminal.
static RESIZED: AtomicBool = AtomicBool::new(false);

/// The owner's exec id; can't collide with a joiner's `attach-<n>`.
const OWNER_ATTACH: &str = "attach-owner";

pub struct FrameworkEngine {
  /// One directory per container, holding its control socket.
  runtime_dir: PathBuf,
  store: Store,
}

impl FrameworkEngine {
  pub fn new(runtime_dir: impl Into<PathBuf>, store: Store) -> Self {
    Self {
      runtime_dir: runtime_dir.into(),
      store,
    }
  }

  fn socket(&self, name: &str) -> PathBuf {
    control::socket_path(&self.runtime_dir.join(name))
  }

  /// Whether this process is the one holding the VM.
  fn owns(&self, name: &str) -> bool {
    ffi::compostbin_is_running(name)
  }

  /// Runs a guest process against the caller's stdio, as the owner of the VM.
  fn attach(name: &str, id: &str, request: &control::Request, stdio: &Stdio) -> i32 {
    ffi::compostbin_exec(
      name,
      id,
      &spec::lines(&request.arguments),
      &spec::lines(&request.environment),
      request.user.as_deref().unwrap_or(""),
      &request.working_directory,
      stdio.terminal,
      stdio.stdin,
      stdio.stdout,
      stdio.stderr,
    )
  }

  /// Serves attaches from other callers on a detached thread, never joined:
  /// it ends with the process, as does the VM.
  fn serve_control_socket(&self, name: String) -> Result<(), EngineError> {
    let path = self.socket(&name);
    let listener = control::bind(&path).map_err(|error| EngineError::failed("bind the control socket", error))?;

    std::thread::spawn(move || {
      control::serve(
        &listener,
        // Use the client's resolved request as-is; never substitute this
        // process's environment.
        |request, stdio, id| Self::attach(&name, id, request, stdio),
        |id, terminal| {
          let _ = ffi::compostbin_resize(id, terminal);
        },
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
    let attached = spec.tty && terminal::is_tty(descriptor) && terminal::is_tty(libc::STDOUT_FILENO);

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
      watch_for_resize();
      Stdio::terminal(descriptor, libc::STDOUT_FILENO)
    } else {
      Stdio::inherit(spec.interactive)
    };

    // The owner attaches directly; anyone else asks the owner to.
    if self.owns(&spec.name) {
      // Duplicates, since the Swift side closes what it is given.
      let duplicated = stdio
        .try_clone()
        .map_err(|error| EngineError::failed("duplicate the caller's stdio", error))?;

      let code = Self::attach(&spec.name, OWNER_ATTACH, &request, &duplicated);

      return checked(code).map_err(|error| EngineError::failed("exec", error));
    }

    // Not duplicated: `SCM_RIGHTS` already copies each one, and the owner dups
    // again.
    control::request(&self.socket(&spec.name), &request, &stdio, &resized)
      .map_err(|error| EngineError::failed("attach", error))
  }

  /// Read from the store's index; there is no daemon to ask.
  fn images(&self) -> Result<Vec<String>, EngineError> {
    self
      .store
      .images()
      .map_err(|error| EngineError::unavailable("read the image index", error))
  }

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError> {
    // The VM dies with its process, so nothing cleans up the previous run's
    // container directory. Start from a fresh rootfs clone, matching
    // `Session::start`'s delete-stopped-container semantics.
    let _ = std::fs::remove_dir_all(self.store.container_dir(&spec.name));

    let code = ffi::compostbin_boot(
      &spec.name,
      &self.store.root().display().to_string(),
      &self.store.kernel().display().to_string(),
      INITFS_REFERENCE,
      &spec.image,
      spec.resources.cpus as i32,
      spec.resources.memory_in_bytes,
      &spec::lines(&spec::mounts(&spec.mounts)),
      &spec::lines(&spec::sockets(&spec.sockets)),
      &spec::lines(&spec::environment(&spec.env)),
      &spec::lines(&spec.arguments),
      &spec::working_directory(spec.workdir.as_deref()),
      &spec::nat_address(&spec.name),
      spec::NAT_GATEWAY,
    );

    checked(code).map_err(|error| EngineError::failed("boot", error))?;
    self.serve_control_socket(spec.name.clone())
  }

  fn is_running(&self, name: &str) -> Result<bool, EngineError> {
    // A socket file with nothing answering is a container that died with its
    // owner.
    Ok(control::served(&self.socket(name)))
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    let code = ffi::compostbin_is_unpacked(&self.store.root().display().to_string(), image);

    Ok(checked(code).map_err(|error| EngineError::failed("find the image", error))? == 1)
  }

  /// Names both boot artefacts: pinned separately, and a mismatch fails only
  /// at runtime.
  fn version(&self) -> Result<Option<String>, EngineError> {
    Ok(Some(format!(
      "Containerization {}, kernel {}",
      crate::INITFS_VERSION,
      crate::KERNEL_VERSION
    )))
  }
}

/// Installs the SIGWINCH handler, once.
fn watch_for_resize() {
  static INSTALLED: std::sync::Once = std::sync::Once::new();

  INSTALLED.call_once(|| {
    // SAFETY: `note_resize` only stores into an atomic, which is
    // async-signal-safe.
    unsafe {
      libc::signal(libc::SIGWINCH, note_resize as *const () as libc::sighandler_t);
    }
  });
}

extern "C" fn note_resize(_signal: libc::c_int) {
  RESIZED.store(true, Ordering::Relaxed);
}

/// Whether the window changed size since last asked. Non-blocking, so the
/// poller can stop when its attach ends.
fn resized() -> bool {
  RESIZED.swap(false, Ordering::Relaxed)
}
