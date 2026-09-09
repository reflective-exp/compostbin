//! How a diagnosis is printed. Core decides what is wrong and what to do about
//! it, the same wherever it is reported; columns and indentation mean something
//! only in a terminal, so they stop here.

use compostbin_core::doctor::{Check, Status};
use std::fmt::{Display, Formatter, Result};

/// Every check, as the terminal shows it. A newtype because `Display` cannot be
/// implemented on core's `Check` here; it also makes the whole report one value
/// a test can assert on.
pub struct Diagnosis<'a>(pub &'a [Check]);

impl Display for Diagnosis<'_> {
  fn fmt(&self, formatter: &mut Formatter<'_>) -> Result {
    for check in self.0 {
      let label = match check.status {
        Status::Fail => "FAIL",
        Status::Ok => "ok  ",
        Status::Warn => "warn",
      };

      writeln!(formatter, "{label}  {}: {}", check.name, check.detail)?;

      // Indented under the check rather than aligned past the label: a finding
      // is usually a path, and padding costs it columns.
      for item in &check.items {
        writeln!(formatter, "    {item}")?;
      }
    }

    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn check(name: &str, status: Status, detail: &str, items: &[&str]) -> Check {
    Check {
      detail: detail.to_string(),
      items: items.iter().map(|item| item.to_string()).collect(),
      name: name.to_string(),
      status,
    }
  }

  #[test]
  fn prints_a_check_without_findings_on_one_line() {
    assert_eq!(
      Diagnosis(&[check("daemon", Status::Ok, "responding", &[])]).to_string(),
      "ok    daemon: responding\n"
    );
  }

  /// Why `items` exists: a check about several paths is unreadable as one line.
  #[test]
  fn prints_each_finding_on_its_own_line_under_the_check() {
    assert_eq!(
      Diagnosis(&[check(
        "host commands",
        Status::Warn,
        "these run on the host as you",
        &["fmt = cargo fmt --all", "test = cargo nextest run --workspace"],
      )])
      .to_string(),
      "warn  host commands: these run on the host as you\n    fmt = cargo fmt --all\n    test = cargo nextest run --workspace\n"
    );
  }
}
