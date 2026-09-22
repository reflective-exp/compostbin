#![cfg(feature = "integration")]
//! What a session can see, and where.
//!
//! The mount table is built on the host and asserted on there by unit tests;
//! what those cannot say is whether the guest really ends up with the tree they
//! describe — that a read-only mount refuses a write, that an edit lands on the
//! host rather than in the container, that nothing undeclared is reachable.

use compostbin_core::session::CLAUDE_HOME_TARGET;
use compostbin_test::{Project, stderr};

#[test]
fn project_is_the_workdir_and_writes_reach_the_host() {
  let project = Project::new("cbt-project-dir");
  project.write("from-the-host.txt", "written on the host\n");

  let seen = project.guest_output(
    "pwd \
     && cat from-the-host.txt \
     && echo 'written in the guest' > from-the-guest.txt",
  );

  assert_eq!(
    seen,
    format!("/workspace/{}\nwritten on the host", "cbt-project-dir"),
    "the session starts in the project, which is mounted under its own name"
  );
  assert_eq!(
    project.read("from-the-guest.txt"),
    "written in the guest\n",
    "an edit in the guest lands directly on the host, with nothing to sync"
  );
}

#[test]
fn extra_path_mounts_under_its_basename() {
  let project = Project::new("cbt-extra-path");
  let libfoo = project.home().join("libfoo");
  std::fs::create_dir(&libfoo).expect("create libfoo");
  std::fs::write(libfoo.join("marker"), "a sibling checkout\n").expect("write marker");

  project.manifest(&format!(
    r#"
[[paths]]
source = "{}"
"#,
    libfoo.display()
  ));

  assert_eq!(
    project.guest_output("cat /workspace/libfoo/marker && echo written > /workspace/libfoo/new && echo ok"),
    "a sibling checkout\nok",
    "a declared path is mounted, and writable unless it says otherwise"
  );
  assert_eq!(std::fs::read_to_string(libfoo.join("new")).expect("read"), "written\n");
}

#[test]
fn readonly_path_refuses_writes() {
  let project = Project::new("cbt-readonly");
  let notes = project.home().join("notes");
  std::fs::create_dir(&notes).expect("create notes");
  std::fs::write(notes.join("kept"), "read me\n").expect("write note");

  project.manifest(&format!(
    r#"
[[paths]]
readonly = true
source = "{}"
"#,
    notes.display()
  ));

  assert_eq!(
    project.guest_output("cat /workspace/notes/kept"),
    "read me",
    "read-only is still readable"
  );

  let refused = project.guest("echo scribble > /workspace/notes/kept");

  assert!(!refused.status.success(), "a read-only mount must refuse a write");
  assert!(
    stderr(&refused).contains("Read-only file system"),
    "the guest should be told why: {}",
    stderr(&refused)
  );
  assert_eq!(
    std::fs::read_to_string(notes.join("kept")).expect("read"),
    "read me\n",
    "and the host file is untouched"
  );
}

/// `target` is for the rare thing that has to appear at a fixed location.
/// Everything else lands under `/workspace/<basename>`, which is what keeps the
/// host's layout out of the container.
#[test]
fn a_declared_target_is_where_the_path_lands() {
  let project = Project::new("cbt-target");
  let vendor = project.home().join("vendor");
  std::fs::create_dir(&vendor).expect("create vendor");
  std::fs::write(vendor.join("marker"), "at a fixed path\n").expect("write marker");

  project.manifest(&format!(
    r#"
[[paths]]
source = "{}"
target = "/opt/vendor"
"#,
    vendor.display()
  ));

  assert_eq!(
    project.guest_output("cat /opt/vendor/marker"),
    "at a fixed path",
    "a declared target is an absolute guest path, not a name under /workspace"
  );
  assert_eq!(
    project.guest_output("ls /workspace"),
    "cbt-target",
    "and a path that named one is not also mounted under its basename"
  );
}

/// The uncommitted overlay is part of the mount table like any other entry:
/// `add --local` writes it, and a session that loads the manifest mounts what
/// it names, with the flags it named.
#[test]
fn the_local_manifest_mounts_too() {
  let project = Project::new("cbt-local");
  let shared = project.home().join("shared");
  let scratch = project.home().join("scratch");
  std::fs::create_dir(&shared).expect("create shared");
  std::fs::create_dir(&scratch).expect("create scratch");
  std::fs::write(scratch.join("marker"), "mine alone\n").expect("write marker");

  project.manifest(&format!(
    r#"
[[paths]]
source = "{}"
"#,
    shared.display()
  ));
  project.write(
    ".config/compostbin.local.toml",
    &format!(
      r#"
[[paths]]
readonly = true
source = "{}"
"#,
      scratch.display()
    ),
  );

  assert_eq!(
    project.guest_output("cat /workspace/scratch/marker"),
    "mine alone",
    "a path only the overlay names is mounted all the same"
  );
  assert!(
    !project
      .guest("echo scribble > /workspace/scratch/marker")
      .status
      .success(),
    "with the flags the overlay gave it"
  );
  assert_eq!(
    project.guest_output("ls /workspace | tr '\\n' ' '"),
    "cbt-local scratch shared",
    "the overlay adds to the committed manifest rather than replacing it"
  );
}

/// A project inside a `[workspace] roots` tree is not mounted twice: it is
/// reached through the root, which is what `add` says when it records nothing.
#[test]
fn project_inside_a_root_mounts_once() {
  let project = Project::new("cbt-in-root");
  let root = project.home().to_path_buf();
  project.manifest(&format!(
    r#"
[workspace]
roots = ["{}"]
"#,
    root.display()
  ));

  let basename = root
    .file_name()
    .expect("the home has a basename")
    .to_string_lossy()
    .into_owned();

  assert_eq!(
    project.guest_output("pwd"),
    format!("/workspace/{basename}/cbt-in-root"),
    "the root is the mount; the project is a directory inside it"
  );
}

/// The conversation `--continue` resumes lives in the home, not the container.
#[test]
fn claude_home_outlives_the_container() {
  let project = Project::new("cbt-claude-home");

  project.guest_output(&format!("echo remembered > {CLAUDE_HOME_TARGET}/marker"));

  assert_eq!(
    std::fs::read_to_string(project.state_dir().join("claude-home/marker")).expect("read the marker"),
    "remembered\n",
    "the guest's Claude home is a directory on the host that the container only borrows"
  );
  // A second `exec` is a second container: the first died with the process that
  // created it.
  assert_eq!(
    project.guest_output(&format!("cat {CLAUDE_HOME_TARGET}/marker")),
    "remembered",
    "the next container starts with the last one's home"
  );
}

#[test]
fn nothing_undeclared_is_mounted() {
  let project = Project::new("cbt-undeclared");
  let elsewhere = project.home().join("elsewhere");
  std::fs::create_dir(&elsewhere).expect("create elsewhere");
  std::fs::write(elsewhere.join("secret"), "not for the guest\n").expect("write secret");

  assert_eq!(
    project.guest_output("ls /workspace"),
    "cbt-undeclared",
    "only what the manifest declares is under /workspace"
  );
  assert!(
    !project
      .guest(&format!("cat {}/secret", elsewhere.display()))
      .status
      .success(),
    "a host path outside every mount is not reachable by its host path either"
  );
}
