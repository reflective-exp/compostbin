//! A pseudo-terminal, for the half of compostbin that only exists when the
//! caller has one.
//!
//! `exec -t`, `compostbin shell`, and a `tty = true` host command all decide
//! what to do by asking whether this process's stdin and stdout are terminals.
//! A test process's are pipes, so none of those paths is reachable without a
//! terminal of the test's own — which is what this is.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;

/// What this terminal reports as its size. Nothing here reads it, but the 0x0 a
/// fresh pty would otherwise report makes anything drawing a UI behave oddly.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

/// A command running on a terminal of its own.
///
/// Both ends of the pty matter: the command's three descriptors are the slave,
/// and the test reads and writes the master. What comes back is one stream, not
/// two — a terminal is one device, so a command's stdout and stderr arrive
/// interleaved on it, and that is the point of running one here.
pub struct Terminal {
  /// Taken by [`Terminal::finish`]; whatever is left when this is dropped is
  /// killed, since the process on the other end owns a container.
  child: Option<Child>,
  master: File,
  reader: Option<JoinHandle<String>>,
}

impl Terminal {
  /// Runs `command` with a terminal on all three of its descriptors.
  pub fn attach(mut command: Command) -> Self {
    let (master, slave) = open();

    command
      .stdin(stdio(&slave))
      .stdout(stdio(&slave))
      .stderr(stdio(&slave));

    let raw = slave.as_raw_fd();

    // SAFETY: pre_exec runs between fork and exec, where only async-signal-safe
    // calls are allowed; setsid and ioctl are. `raw` survives the fork with the
    // inherited descriptor table.
    unsafe {
      command.pre_exec(move || {
        if libc::setsid() < 0 {
          return Err(io::Error::last_os_error());
        }

        if libc::ioctl(raw, libc::TIOCSCTTY as _, 0) < 0 {
          return Err(io::Error::last_os_error());
        }

        Ok(())
      });
    }

    let child = command.spawn().expect("the command should start");
    // Dropped as soon as the child has its own: a read of the master blocks
    // while any copy of the slave is open, ours included, so keeping one would
    // mean never seeing the end of the output.
    drop(slave);

    let duplicate = File::from(master.try_clone().expect("duplicate the terminal"));

    Self {
      child: Some(child),
      master: File::from(master),
      reader: Some(std::thread::spawn(move || drain(duplicate))),
    }
  }

  /// Types a line, as a person at this terminal would.
  pub fn send(&mut self, line: &str) {
    writeln!(self.master, "{line}").expect("write to the terminal");
    self.master.flush().expect("flush the terminal");
  }

  /// Waits for the command and reports everything the terminal showed.
  pub fn finish(mut self) -> (String, ExitStatus) {
    let status = self
      .child
      .take()
      .expect("a terminal is finished once")
      .wait()
      .expect("the command should finish");

    // Joined after the wait, not before: the reader ends when the last slave
    // closes, which is when the command exits.
    let shown = self
      .reader
      .take()
      .expect("a terminal is finished once")
      .join()
      .expect("the reader should not panic");

    (shown, status)
  }
}

impl Drop for Terminal {
  /// Killing what is left matters more here than for a plain child: the process
  /// on the other end of this terminal owns a container, which would otherwise
  /// outlive the test as a VM nothing is going to stop.
  fn drop(&mut self) {
    if let Some(mut child) = self.child.take() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

/// A fresh pty, as its master and slave.
fn open() -> (OwnedFd, OwnedFd) {
  let mut master = 0;
  let mut slave = 0;
  // `*mut` in the signature even though openpty only reads it.
  let mut size = libc::winsize {
    ws_row: ROWS,
    ws_col: COLUMNS,
    ws_xpixel: 0,
    ws_ypixel: 0,
  };

  // SAFETY: openpty writes two valid descriptors, or returns < 0 and writes
  // neither.
  let opened = unsafe {
    libc::openpty(
      &mut master,
      &mut slave,
      std::ptr::null_mut(),
      std::ptr::null_mut(),
      &mut size,
    )
  };

  assert!(opened >= 0, "could not open a pty: {}", io::Error::last_os_error());

  // SAFETY: both descriptors are freshly opened and owned by us alone.
  unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

fn stdio(slave: &OwnedFd) -> Stdio {
  Stdio::from(slave.try_clone().expect("duplicate the terminal"))
}

/// Reads the terminal until the command has closed it.
///
/// A master whose last slave has gone reports that as end of file on some
/// systems and as `EIO` on others; both mean the command is done.
fn drain(mut terminal: File) -> String {
  let mut shown = Vec::new();
  let mut chunk = [0; 4096];

  loop {
    match terminal.read(&mut chunk) {
      Ok(0) => break,
      Ok(read) => shown.extend_from_slice(&chunk[..read]),
      Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
      Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
      Err(error) => panic!("reading the terminal failed: {error}"),
    }
  }

  String::from_utf8_lossy(&shown).into_owned()
}
