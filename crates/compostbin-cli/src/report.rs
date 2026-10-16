use crate::engine::select;
use compostbin_core::doctor::{self, Check, Status};
use compostbin_core::session::Session;
use compostbin_core::session::credentials::Keychain;
use std::error::Error;
use std::fmt;

pub fn report_diagnosis(session: &Session) -> Result<i32, Box<dyn Error>> {
  let checks = doctor::diagnose(
    session,
    &select(session)?,
    &Keychain,
    std::env::var_os("ANTHROPIC_API_KEY").is_some(),
  );

  print!("{}", Diagnosis(&checks));

  Ok(i32::from(checks.iter().any(|check| check.status == Status::Fail)))
}

pub struct Diagnosis<'a>(pub &'a [Check]);

impl fmt::Display for Diagnosis<'_> {
  fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    for check in self.0 {
      let label = match check.status {
        Status::Fail => "FAIL",
        Status::Ok => "ok  ",
        Status::Warn => "warn",
      };

      writeln!(formatter, "{label}  {}: {}", check.name, check.detail)?;

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

  fn check(name: &'static str, status: Status, detail: &str, items: &[&str]) -> Check {
    Check {
      detail: detail.to_string(),
      items: items.iter().map(|item| item.to_string()).collect(),
      name,
      status,
    }
  }

  #[test]
  fn prints_a_check_without_findings_on_one_line() {
    assert_eq!(
      Diagnosis(&[check("image store", Status::Ok, "readable", &[])]).to_string(),
      "ok    image store: readable\n"
    );
  }

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
