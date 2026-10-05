//! nextest's setup script for the integration suite; see `compostbin_test::prepare`.
//! With `--remove-after <pid> <directory>`, the process it leaves to clean up.

use std::path::Path;

fn main() {
  let arguments: Vec<String> = std::env::args().skip(1).collect();

  match arguments.as_slice() {
    [] => compostbin_test::prepare(),
    [flag, pid, directory] if flag == compostbin_test::REMOVE_AFTER => {
      compostbin_test::remove_after(pid.parse().expect("a pid"), Path::new(directory));
    }
    _ => panic!(
      "usage: prepare-store [{} <pid> <directory>]",
      compostbin_test::REMOVE_AFTER
    ),
  }
}
