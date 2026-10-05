//! The `install` command: compostbin's skills in ~/.claude.

use compostbin_core::skills::{self, Change};
use std::error::Error;
use std::io::{IsTerminal, Write};

/// Shows what installing would change and applies it, asking first unless
/// `yes`.
pub fn install(changes: &[Change], yes: bool) -> Result<i32, Box<dyn Error>> {
  if changes.is_empty() {
    println!("the skills and the profiles directory are up to date");
    return Ok(0);
  }

  for change in changes {
    println!("{change}");
  }

  if !yes {
    if !std::io::stdin().is_terminal() {
      eprintln!("compostbin: pass --yes to install without a terminal to ask on");
      return Ok(1);
    }

    print!("apply? [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;

    if !matches!(answer.trim(), "y" | "Y" | "yes") {
      println!("nothing changed");
      return Ok(1);
    }
  }

  skills::apply(changes)?;
  println!("installed");

  Ok(0)
}
