//! Reading what a finished command left behind.

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
  output.status.code().unwrap_or(-1)
}
