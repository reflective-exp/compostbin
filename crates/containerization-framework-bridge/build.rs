//! Generates the FFI glue, builds the Swift package, and tells cargo how to
//! link it.
//!
//! macOS only. Elsewhere the engine and builder are stand-ins
//! (`unsupported.rs`) and this does nothing, so the workspace still checks on
//! Linux.

use std::path::PathBuf;
use std::process::Command;

/// swift-bridge's generated header directory; `bridging-header.h` imports it.
const BRIDGE: &str = "containerization-bridge";
const PACKAGE: &str = "ContainerizationBridge";

fn manifest_dir() -> PathBuf {
  PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
}

fn swift_package_dir() -> PathBuf {
  manifest_dir()
    .join("swift")
    .canonicalize()
    .expect("the swift package is committed inside this crate")
}

fn swift_source_dir() -> PathBuf {
  swift_package_dir().join("Sources").join(PACKAGE)
}

fn is_release() -> bool {
  std::env::var("PROFILE").as_deref() == Ok("release")
}

/// Makes swift-bridge's generated `@_cdecl` shims public.
///
/// They are emitted internal. A release build optimises whole-module, sees no
/// caller inside the module (the callers are across the C ABI), and strips
/// them, leaving two undefined `__swift_bridge__$…` symbols at link time.
/// Debug builds don't optimise, so `cargo test` never shows it.
fn publish_bridge_shims(generated: &std::path::Path) {
  let path = generated.join(BRIDGE).join(format!("{BRIDGE}.swift"));
  let source = std::fs::read_to_string(&path).expect("swift-bridge should have just written the glue");
  let published = source.replace("\nfunc __swift_bridge__", "\npublic func __swift_bridge__");

  // A silent no-op would resurface as an unexplained release-only link failure.
  if published == source {
    panic!(
      "no `func __swift_bridge__` to publish in {}; swift-bridge's output has changed shape",
      path.display()
    );
  }

  std::fs::write(&path, published).expect("the generated glue should be writable");
}

/// Copies generated glue into the package, skipping unchanged files and
/// removing files this run didn't write.
///
/// SwiftPM and cargo's `rerun-if-changed` both key off mtimes, so rewriting
/// identical bytes would recompile the module and rerun this script. SwiftPM
/// compiles every file in the directory, so glue from an earlier run has to
/// go: it calls a bridge that no longer exists.
fn sync_generated(staged: &std::path::Path, generated: &std::path::Path) {
  std::fs::create_dir_all(generated).expect("the generated directory should be creatable");

  for entry in std::fs::read_dir(generated).expect("the generated directory was just created") {
    let entry = entry.expect("a readable directory entry");

    if staged.join(entry.file_name()).exists() {
      continue;
    }

    let stale = entry.path();
    let removed = if entry.file_type().expect("a stat-able entry").is_dir() {
      std::fs::remove_dir_all(&stale)
    } else {
      std::fs::remove_file(&stale)
    };

    removed.expect("stale generated glue should be removable");
  }

  for entry in std::fs::read_dir(staged).expect("swift-bridge should have just written the glue") {
    let entry = entry.expect("a readable directory entry");
    let destination = generated.join(entry.file_name());

    if entry.file_type().expect("a stat-able entry").is_dir() {
      sync_generated(&entry.path(), &destination);
      continue;
    }

    let fresh = std::fs::read(entry.path()).expect("a readable generated file");

    if std::fs::read(&destination).is_ok_and(|current| current == fresh) {
      continue;
    }

    std::fs::write(&destination, fresh).expect("the generated glue should be writable");
  }
}

/// Compiles the Swift package to a static library.
///
/// The bridging header is set in the package's `swiftSettings`, not via
/// `-Xswiftc`, which SwiftPM would apply to every target in the graph.
fn compile_swift() {
  let mut command = Command::new("swift");

  command.current_dir(swift_package_dir()).arg("build");

  if is_release() {
    command.args(["-c", "release"]);
  }

  let output = command.output().expect("swift should be on PATH on macOS");

  if !output.status.success() {
    panic!(
      "swift build failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
      String::from_utf8_lossy(&output.stdout),
      String::from_utf8_lossy(&output.stderr),
    );
  }
}

/// Where `swift build` leaves the static library.
fn swift_build_dir() -> PathBuf {
  swift_package_dir()
    .join(".build")
    .join(if is_release() { "release" } else { "debug" })
}

/// System libraries Containerization's `CArchive` target links against.
///
/// Its `linkerSettings` only apply when SwiftPM links; without repeating them
/// here every `archive_*` symbol is undefined in the Rust binary.
const SYSTEM_LIBRARIES: [&str; 5] = ["archive", "z", "bz2", "lzma", "iconv"];

/// The Swift runtime the static library depends on but does not carry.
fn link_swift_runtime() {
  let developer = Command::new("xcode-select")
    .arg("--print-path")
    .output()
    .ok()
    .filter(|output| output.status.success())
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    .unwrap_or_else(|| "/Applications/Xcode.app/Contents/Developer".to_string());

  println!("cargo:rustc-link-search={developer}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx/");
  println!("cargo:rustc-link-search=/usr/lib/swift");

  // The Swift runtime (notably libswift_Concurrency) is dynamic and referenced
  // as `@rpath/...`. Without an rpath the binary links but fails in dyld at
  // launch.
  println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
  println!("cargo:rustc-link-lib=framework=Virtualization");
  // `SecTaskCopyValueForEntitlement`: the Swift side checks the build is
  // signed before starting a VM.
  println!("cargo:rustc-link-lib=framework=Security");

  for library in SYSTEM_LIBRARIES {
    println!("cargo:rustc-link-lib={library}");
  }
}

fn main() {
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=src/bridge.rs");

  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }

  println!("cargo:rerun-if-changed={}", swift_source_dir().display());
  println!(
    "cargo:rerun-if-changed={}",
    swift_package_dir().join("Package.swift").display()
  );

  let staged = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("swift-bridge");

  // `OUT_DIR` survives between builds; clearing it leaves only this run's glue.
  let _ = std::fs::remove_dir_all(&staged);

  swift_bridge_build::parse_bridges(vec![manifest_dir().join("src/bridge.rs")]).write_all_concatenated(&staged, BRIDGE);
  publish_bridge_shims(&staged);
  sync_generated(&staged, &swift_source_dir().join("generated"));

  compile_swift();

  println!("cargo:rustc-link-lib=static={PACKAGE}");
  println!("cargo:rustc-link-search={}", swift_build_dir().display());

  link_swift_runtime();
}
