//! `Engine`, backed by Containerization.framework.
//!
//! A container lives *in* this process, not a daemon. So:
//!
//! - The VM lives exactly as long as this process; `run` also starts the
//!   control socket through which `shell` in another terminal reaches it.
//! - Nothing lists containers. A container is up iff something answers on its
//!   control socket (one per container, under the runtime directory).

use crate::store::{INITFS_REFERENCE, Store};
use crate::{checked, control, ffi, spec, terminal};
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{ExecSpec, RunSpec};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGWINCH handler. One per process: a process attaches at most
/// one terminal.
static RESIZED: AtomicBool = AtomicBool::new(false);

/// `run`'s exec id; can't collide with `shell`'s `attach-<n>`.
const OWNER_ATTACH: &str = "attach-owner";

const NO_TERMINAL: RawFd = -1;

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

  /// Runs a guest process against a terminal, as the owner of the VM.
  fn attach(name: &str, id: &str, request: &control::Request, terminal: RawFd) -> i32 {
    ffi::compostbin_exec(
      name,
      id,
      &spec::lines(&request.arguments),
      &spec::lines(&request.environment),
      request.user.as_deref().unwrap_or(""),
      &request.working_directory,
      terminal,
    )
  }

  /// Serves attaches from other terminals on a detached thread, never joined:
  /// it ends with the process, as does the VM.
  fn serve_control_socket(&self, name: String) -> Result<(), EngineError> {
    let path = self.socket(&name);
    let listener = control::bind(&path).map_err(|error| EngineError::failed("bind the control socket", error))?;

    std::thread::spawn(move || {
      control::serve(
        &listener,
        // Use the client's resolved request as-is; never substitute this
        // process's environment.
        |request, terminal, id| Self::attach(&name, id, request, terminal),
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
    let attached = terminal::is_tty(descriptor);

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

    watch_for_resize();

    // The owner attaches directly; anyone else asks the owner to.
    if self.owns(&spec.name) {
      // A duplicate, since Swift closes what it's given and we keep stdin. A
      // non-terminal isn't handed over; the guest runs without one.
      let lent = if attached {
        terminal::lend(descriptor).map_err(|error| EngineError::failed("lend the terminal", error))?
      } else {
        NO_TERMINAL
      };

      let code = Self::attach(&spec.name, OWNER_ATTACH, &request, lent);

      return checked(code).map_err(|error| EngineError::failed("exec", error));
    }

    if !attached {
      return Err(EngineError::failed(
        "attach",
        "a session can only be attached from a terminal",
      ));
    }

    // Not duplicated: `SCM_RIGHTS` already copies it, and the owner dups again.
    control::request(&self.socket(&spec.name), &request, descriptor, &resized)
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
