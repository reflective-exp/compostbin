//! Generates the FFI glue, builds the Swift package, and tells cargo how to
//! link it.
//!
//! The package is staged into `OUT_DIR`, at
//! `target/<profile>/containerization-framework-swift`.
//!
//! SwiftPM's build directory stays out of `OUT_DIR`, which is keyed by this
//! script's fingerprint — editing this file, or the rustflags cargo was invoked
//! with, yields a new one. That directory holds the compiled dependency graph, so
//! a copy per fingerprint costs ~2 GiB and a full rebuild of Containerization.
//!
//! macOS only. On other platforms the session and builder are stand-ins
//! (`unsupported.rs`) that compile, but return errors on every invocation.

use std::path::{Path, PathBuf};
use std::process::Command;

/// swift-bridge's generated header directory; `bridging-header.h` imports it.
const BRIDGE: &str = "containerization-bridge";
const PACKAGE: &str = "ContainerizationBridge";

/// Never copied into the staged package: SwiftPM's build directory, and glue
/// this script regenerates.
const NOT_INPUTS: [&str; 2] = [".build", "generated"];

fn manifest_dir() -> PathBuf {
  PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
}

fn out_dir() -> PathBuf {
  PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
}

fn source_package_dir() -> PathBuf {
  manifest_dir()
    .join("swift")
    .canonicalize()
    .expect("the swift package is committed inside this crate")
}

fn source_sources_dir() -> PathBuf {
  source_package_dir().join("Sources").join(PACKAGE)
}

fn staged_package_dir() -> PathBuf {
  out_dir().join("swift")
}

/// SwiftPM's build directory, one per profile.
///
/// `OUT_DIR` is `<target>/<profile>/build/<crate>-<fingerprint>/out`, so the
/// profile directory is its fourth ancestor. Cargo documents no part of that
/// layout, so a path that doesn't match it falls back to `OUT_DIR`.
fn scratch_dir() -> PathBuf {
  let out = out_dir();
  let profile = out.ancestors().nth(3).filter(|_| {
    out
      .ancestors()
      .nth(2)
      .is_some_and(|dir| dir.ends_with("build"))
  });

  profile
    .unwrap_or(&out)
    .join("containerization-framework-swift")
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
fn publish_bridge_shims(generated: &Path) {
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

/// Mirrors `from` onto `to`, skipping unchanged files and removing anything
/// this run didn't write. SwiftPM keys off mtimes.
fn mirror(from: &Path, to: &Path, skip: &[&str]) {
  std::fs::create_dir_all(to).expect("the destination should be creatable");

  for entry in std::fs::read_dir(to).expect("the destination was just created") {
    let entry = entry.expect("a readable directory entry");

    if from.join(entry.file_name()).exists() {
      continue;
    }

    let stale = entry.path();
    let removed = if entry.file_type().expect("a stat-able entry").is_dir() {
      std::fs::remove_dir_all(&stale)
    } else {
      std::fs::remove_file(&stale)
    };

    removed.expect("a stale staged file should be removable");
  }

  for entry in std::fs::read_dir(from).expect("the source directory should be readable") {
    let entry = entry.expect("a readable directory entry");
    let name = entry.file_name();

    if skip.iter().any(|skipped| name == *skipped) {
      continue;
    }

    let destination = to.join(&name);

    if entry.file_type().expect("a stat-able entry").is_dir() {
      mirror(&entry.path(), &destination, skip);
      continue;
    }

    let fresh = std::fs::read(entry.path()).expect("a readable source file");

    if std::fs::read(&destination).is_ok_and(|current| current == fresh) {
      continue;
    }

    std::fs::write(&destination, fresh).expect("the staged file should be writable");
  }
}

/// Compiles the staged package to a static library.
///
/// The bridging header is set in the package's `swiftSettings`, not via
/// `-Xswiftc`, which SwiftPM would apply to every target in the graph. It
/// resolves against `#filePath`, so it follows the package to `OUT_DIR`.
///
/// The scratch path holds every dependency's checkout as well as the compiled
/// output, hydrated from SwiftPM's cache.
fn compile_swift() {
  let mut command = Command::new("swift");

  command
    .arg("build")
    .arg("--package-path")
    .arg(staged_package_dir())
    .arg("--scratch-path")
    .arg(scratch_dir());

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
  scratch_dir().join(if is_release() { "release" } else { "debug" })
}

/// System libraries Containerization's `CArchive` target links against.
///
/// Its `linkerSettings` only apply when SwiftPM links; without repeating them
/// here every `archive_*` symbol is undefined in the Rust binary.
const SYSTEM_LIBRARIES: [&str; 5] = ["archive", "z", "bz2", "lzma", "iconv"];

/// Where the dynamic Swift runtime lives.
const SWIFT_RUNTIME_DIR: &str = "/usr/lib/swift";

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
  println!("cargo:rustc-link-search={SWIFT_RUNTIME_DIR}");

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

  println!("cargo:rerun-if-changed={}", source_sources_dir().display());
  println!(
    "cargo:rerun-if-changed={}",
    source_package_dir().join("Package.swift").display()
  );

  let staged = staged_package_dir();

  mirror(&source_package_dir(), &staged, &NOT_INPUTS);

  let glue = out_dir().join("swift-bridge");

  // `OUT_DIR` survives between builds.
  let _ = std::fs::remove_dir_all(&glue);

  swift_bridge_build::parse_bridges(vec![manifest_dir().join("src/bridge.rs")]).write_all_concatenated(&glue, BRIDGE);
  publish_bridge_shims(&glue);
  mirror(&glue, &staged.join("Sources").join(PACKAGE).join("generated"), &[]);

  compile_swift();

  println!("cargo:rustc-link-lib=static={PACKAGE}");
  println!("cargo:rustc-link-search={}", swift_build_dir().display());

  link_swift_runtime();
}
