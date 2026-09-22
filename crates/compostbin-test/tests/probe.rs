#![cfg(feature = "integration")]

use compostbin_test::{Project, stderr, stdout};
use std::time::{Duration, Instant};

/// A second host command after `clean`, bounded so a hang reports instead of
/// hanging.
#[test]
fn second_command_after_clean() {
  let project = Project::new("cbt-stdin-probe");
  project.manifest(
    r#"
[host.commands.greet]
argv = ["echo", "still served"]
"#,
  );

  let started = Instant::now();
  let mut guest = project.guest_in_background(
    "compostbin-host greet < /dev/null > before; echo first=$?; read step; \
     timeout 20 compostbin-host greet < /dev/null > after; echo second=$?",
  );

  std::thread::sleep(Duration::from_secs(2));
  let cleaned = project.compostbin(&["clean"]);
  println!("clean: {}", cleaned.status);
  guest.send("go");

  let finished = guest.finish();
  println!(
    "{:?} status {} stdout {:?} stderr {:?}",
    started.elapsed(),
    finished.status,
    stdout(&finished),
    stderr(&finished)
  );
  println!(
    "before {:?} after {:?}",
    std::fs::read_to_string(project.dir().join("before")).ok(),
    std::fs::read_to_string(project.dir().join("after")).ok()
  );
}
