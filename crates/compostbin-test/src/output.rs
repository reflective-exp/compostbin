//! Reading what a finished command left behind.

use compostbin_core::host::SIGNAL_EXIT_BASE;
use std::os::unix::process::ExitStatusExt;
use std::process::Output;

pub fn stdout(output: &Output) -> String {
  String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
  String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The exit code, or the signal a killed process died of, as a shell reports
/// it.
pub fn code(output: &Output) -> i32 {
  let status = output.status;

  status
    .code()
    .or_else(|| status.signal().map(|signal| SIGNAL_EXIT_BASE + signal))
    .expect("a finished process either exited or was killed")
}
