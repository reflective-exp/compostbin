//! A pseudo-terminal for host commands declared `tty = true`.
//!
//! This wires up descriptors; it never changes what is executed. The terminal is
//! deliberately *not* obtained by wrapping the command in `script -c` or
//! `sh -c` — the usual shortcuts — because either would reintroduce shell
//! parsing, and with it the injection the allowlist exists to prevent.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Nothing tells us the guest's real size, and a plausible one formats better
/// than the 0x0 a fresh pty reports.
const COLUMNS: u16 = 120;
const ROWS: u16 = 40;

/// The slave becomes the command's three standard descriptors; its output comes
/// back on the master.
pub struct Pty {
  master: OwnedFd,
  slave: OwnedFd,
}

impl Pty {
  pub fn open() -> io::Result<Self> {
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

    if opened < 0 {
      return Err(io::Error::last_os_error());
    }

    // SAFETY: both descriptors are freshly opened and owned by us alone.
    Ok(unsafe {
      Self {
        master: OwnedFd::from_raw_fd(master),
        slave: OwnedFd::from_raw_fd(slave),
      }
    })
  }

  /// Points the command's three descriptors at the slave and makes it their
  /// controlling terminal, so `isatty` is true and signals reach the command.
  pub fn attach(&self, command: &mut Command) -> io::Result<()> {
    command
      .stdin(self.slave_stdio()?)
      .stdout(self.slave_stdio()?)
      .stderr(self.slave_stdio()?);

    let slave = self.slave.as_raw_fd();

    // SAFETY: pre_exec runs between fork and exec, where only async-signal-safe
    // calls are allowed; setsid and ioctl are. `slave` survives the fork with the
    // inherited descriptor table.
    unsafe {
      command.pre_exec(move || {
        if libc::setsid() < 0 {
          return Err(io::Error::last_os_error());
        }

        if libc::ioctl(slave, libc::TIOCSCTTY as _, 0) < 0 {
          return Err(io::Error::last_os_error());
        }

        Ok(())
      });
    }

    Ok(())
  }

  fn slave_stdio(&self) -> io::Result<Stdio> {
    Ok(Stdio::from(self.slave.try_clone()?))
  }

  /// Dropping the slave here matters: while any copy stays open on our side,
  /// master reads block forever instead of ending when the command exits.
  pub fn into_master(self) -> OwnedFd {
    self.master
  }
}
