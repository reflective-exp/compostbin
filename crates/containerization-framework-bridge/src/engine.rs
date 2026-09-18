//! `Engine`, backed by Containerization.framework.
//!
//! A container lives *in* this process rather than in a daemon's, and two
//! consequences run through everything below.
//!
//! The first is that `run` does not return to a caller that can walk away. The
//! VM lives exactly as long as this process, so `run` also starts the control
//! socket, which is how `shell` in another terminal reaches it.
//!
//! The second is that there is no register to ask. Nothing keeps a list of
//! containers, so the question "is this container up?" is answered by whether
//! anything answers on its control socket — one per container, in a directory
//! named after it under the engine's runtime directory.

use crate::store::Store;
use crate::{checked, control, ffi, spec, terminal};
use compostbin_engine::engine::Engine;
use compostbin_engine::error::EngineError;
use compostbin_engine::model::{ExecSpec, RunSpec};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGWINCH handler. One flag for the process is enough: a terminal
/// attached twice is not a thing compostbin does.
static RESIZED: AtomicBool = AtomicBool::new(false);

/// The exec id `run` attaches under. `shell` gets `attach-<n>` from the control
/// socket, so the two never collide.
const OWNER_ATTACH: &str = "attach-owner";

/// What the bridge reads as "run this without a terminal".
const NO_TERMINAL: RawFd = -1;

pub struct FrameworkEngine {
  /// One directory per container, named after it, holding its control socket.
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

  fn failed(action: &'static str, error: impl std::fmt::Display) -> EngineError {
    EngineError::failed(action, error)
  }

  /// Runs a guest process against a terminal, as the owner of the VM.
  fn attach(name: &str, id: &str, spec: &ExecSpec, terminal: RawFd) -> i32 {
    ffi::compostbin_exec(
      name,
      id,
      &spec::lines(&spec.arguments),
      &spec::environment(&spec.env),
      &spec
        .workdir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "/".to_string()),
      terminal,
    )
  }

  /// Serves `shell` and anything else that attaches from another terminal, for
  /// as long as the VM lives.
  ///
  /// Detached, and never joined: it ends when the process does, which is the
  /// same moment the VM it serves goes away.
  fn serve_control_socket(&self, name: String) -> Result<(), EngineError> {
    let path = self.socket(&name);
    let listener = control::bind(&path).map_err(|error| Self::failed("bind the control socket", error))?;

    std::thread::spawn(move || {
      control::serve(
        &listener,
        |request, terminal, id| {
          // Everything comes from the client, already resolved: it read the
          // same manifest, and this process must not substitute its own
          // environment for the one the caller meant.
          ffi::compostbin_exec(
            &name,
            id,
            &spec::lines(&request.arguments),
            &spec::lines(&request.environment),
            &request.working_directory,
            terminal,
          )
        },
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

    // Raw for as long as the guest is attached, and restored by the drop
    // however this returns. Without it the host tty keeps echoing and line
    // buffering on top of the guest's own pty, which reads as every character
    // doubled and nothing happening until return.
    let _raw = if attached {
      Some(terminal::Raw::acquire(descriptor).map_err(|error| Self::failed("raw mode", error))?)
    } else {
      None
    };

    // The owner attaches directly; anyone else asks the owner to. The guest
    // gets the same terminal either way — the difference is only which process
    // is holding the VM.
    if self.owns(&spec.name) {
      watch_for_resize();

      // A duplicate, because the Swift side closes what it is given and this
      // process needs to keep its own stdin. A descriptor that is not a
      // terminal is not handed over at all: the guest runs without one, which
      // is what `compostbin run > log` should do.
      let lent = if attached {
        terminal::lend(descriptor).map_err(|error| Self::failed("lend the terminal", error))?
      } else {
        NO_TERMINAL
      };

      let code = Self::attach(&spec.name, OWNER_ATTACH, spec, lent);

      return checked(code).map_err(|error| Self::failed("exec", error));
    }

    let request = control::Request {
      arguments: spec.arguments.clone(),
      environment: spec::environment(&spec.env)
        .lines()
        .map(str::to_string)
        .collect(),
      working_directory: spec
        .workdir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "/".to_string()),
    };

    watch_for_resize();

    if !attached {
      return Err(Self::failed("attach", "a session can only be attached from a terminal"));
    }

    // This process's own terminal, not a duplicate: sending a descriptor over
    // the socket copies it, and the owner takes its own duplicate of what
    // arrives. Ours stays ours.
    control::request(&self.socket(&spec.name), &request, descriptor, &resized)
      .map_err(|error| Self::failed("attach", error))
  }

  /// Read from the store's own index rather than asked of anything.
  ///
  /// There is no daemon to ask: what a session can run is what `compostbin
  /// build` has already put on disk, and the index is where it goes.
  fn images(&self) -> Result<Vec<String>, EngineError> {
    self
      .store
      .images()
      .map_err(|error| EngineError::unavailable("read the image index", error))
  }

  fn run(&self, spec: &RunSpec) -> Result<String, EngineError> {
    // Every previous run left a container directory behind: the VM dies with its
    // process, so nothing gets the chance to tidy up afterwards. So a run always
    // begins on a fresh clone of the image's rootfs, which is what
    // `Session::start` deleting a stopped container already meant.
    let _ = std::fs::remove_dir_all(self.store.container_dir(&spec.name));

    let code = ffi::compostbin_boot(
      &spec.name,
      &self.store.root().display().to_string(),
      &self.store.kernel().display().to_string(),
      self.store.initfs_reference(),
      &spec.image,
      spec.resources.cpus as i32,
      spec.resources.memory_in_bytes,
      &spec::mounts(&spec.mounts),
      &spec::sockets(&spec.sockets),
      &spec::environment(&spec.env),
      &spec::lines(&spec.arguments),
      &spec
        .workdir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "/".to_string()),
      &spec::nat_address(&spec.name),
      spec::NAT_GATEWAY,
    );

    checked(code).map_err(|error| Self::failed("boot", error))?;
    self.serve_control_socket(spec.name.clone())?;

    Ok(spec.name.clone())
  }

  fn is_running(&self, name: &str) -> Result<bool, EngineError> {
    // Either we hold it, or whoever does is answering on the socket. A socket
    // file with nothing behind it is a container that died with its owner.
    Ok(control::served(&self.socket(name)))
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    let code = ffi::compostbin_is_unpacked(&self.store.root().display().to_string(), image);

    Ok(checked(code).map_err(|error| Self::failed("find the image", error))? == 1)
  }

  /// Names both halves of what boots a session, because they are pinned
  /// separately and a mismatch is a runtime failure rather than a build one.
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

/// Whether the window has changed size since this was last asked.
///
/// Does not block: the caller polls it, and needs to be able to stop polling
/// when its attach ends.
fn resized() -> bool {
  RESIZED.swap(false, Ordering::Relaxed)
}
