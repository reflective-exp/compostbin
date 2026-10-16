use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;

/// A fresh pty reports 0x0, which makes anything drawing a UI misbehave.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

pub struct Terminal {
  child: Option<Child>,
  master: File,
  reader: Option<JoinHandle<String>>,
}

impl Terminal {
  pub fn attach(mut command: Command) -> Self {
    let (master, slave) = open();

    command
      .stdin(stdio(&slave))
      .stdout(stdio(&slave))
      .stderr(stdio(&slave));

    // SAFETY: setsid and ioctl are async-signal-safe; stdin is already the
    // slave by now, so it becomes the controlling terminal.
    unsafe {
      command.pre_exec(|| {
        if libc::setsid() < 0 {
          return Err(io::Error::last_os_error());
        }

        if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0 {
          return Err(io::Error::last_os_error());
        }

        Ok(())
      });
    }

    let child = command.spawn().expect("the command should start");
    // The master never reports end of output while any copy of the slave is open.
    drop(slave);

    let duplicate = File::from(master.try_clone().expect("duplicate the terminal"));

    Self {
      child: Some(child),
      master: File::from(master),
      reader: Some(std::thread::spawn(move || drain(duplicate))),
    }
  }

  pub fn send(&mut self, line: &str) {
    writeln!(self.master, "{line}").expect("write to the terminal");
    self.master.flush().expect("flush the terminal");
  }

  pub fn finish(mut self) -> (String, ExitStatus) {
    let status = self
      .child
      .take()
      .expect("a terminal is finished once")
      .wait()
      .expect("the command should finish");

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
  fn drop(&mut self) {
    if let Some(mut child) = self.child.take() {
      let _ = child.kill();
      let _ = child.wait();
    }
  }
}

fn open() -> (OwnedFd, OwnedFd) {
  let mut master = 0;
  let mut slave = 0;
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
  let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
  close_on_exec(&master);
  close_on_exec(&slave);

  (master, slave)
}

/// openpty cannot open close-on-exec, and a slave leaked into another process
/// keeps the reader from ever seeing the end of the output.
fn close_on_exec(descriptor: &OwnedFd) {
  // SAFETY: F_SETFD on a descriptor we own touches nothing but its flags.
  let set = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };

  assert!(
    set >= 0,
    "could not mark the pty close-on-exec: {}",
    io::Error::last_os_error()
  );
}

fn stdio(slave: &OwnedFd) -> Stdio {
  Stdio::from(slave.try_clone().expect("duplicate the terminal"))
}

/// A master whose last slave has closed reports end of file on some systems
/// and `EIO` on others.
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
