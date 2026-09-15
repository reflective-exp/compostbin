//! Generates the FFI glue, builds the Swift package, and tells cargo how to
//! link it.
//!
//! Only on macOS. Everywhere else the crate is empty — `#![cfg(target_os =
//! "macos")]` in `lib.rs` — and this does nothing, so the workspace still
//! checks from inside a compostbin session's Debian guest.

use std::path::PathBuf;
use std::process::Command;

/// The name swift-bridge gives the generated header directory, and so the path
/// `bridging-header.h` imports.
const BRIDGE: &str = "compostbin-containerization";
const PACKAGE: &str = "CompostbinContainerization";

fn manifest_dir() -> PathBuf {
  PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"))
}

fn swift_package_dir() -> PathBuf {
  manifest_dir()
    .join("../../swift")
    .join(PACKAGE)
    .canonicalize()
    .expect("the swift package is committed alongside this crate")
}

fn swift_source_dir() -> PathBuf {
  swift_package_dir().join("Sources").join(PACKAGE)
}

fn is_release() -> bool {
  std::env::var("PROFILE").as_deref() == Ok("release")
}

/// Makes swift-bridge's generated `@_cdecl` shims public.
///
/// They are emitted as internal functions. A release build compiles the package
/// whole-module with optimisation, and an internal function that nothing inside
/// the module calls is dead code — so both shims, and the functions behind them,
/// are stripped from the archive and the binary fails to link against two
/// undefined `__swift_bridge__$…` symbols. Debug builds do not optimise, which
/// is the only reason this is not visible from `cargo test`.
///
/// Public is what they already are in substance: they are the library's entire
/// reason to exist, and the only thing that calls them is on the other side of
/// the C ABI, where the optimiser cannot see it.
fn publish_bridge_shims(generated: &std::path::Path) {
  let path = generated.join(BRIDGE).join(format!("{BRIDGE}.swift"));
  let source = std::fs::read_to_string(&path).expect("swift-bridge should have just written the glue");
  let published = source.replace("\nfunc __swift_bridge__", "\npublic func __swift_bridge__");

  // A silent no-op here would come back as an unexplained release-only link
  // failure, so fail where the cause is legible instead.
  if published == source {
    panic!(
      "no `func __swift_bridge__` to publish in {}; swift-bridge's output has changed shape",
      path.display()
    );
  }

  std::fs::write(&path, published).expect("the generated glue should be writable");
}

/// Compiles the Swift package to a static library.
///
/// The bridging header is passed rather than committed into the module: it
/// names files swift-bridge has only just generated, so it cannot be part of
/// the package's own source list.
fn compile_swift() {
  let header = swift_source_dir().join("bridging-header.h");
  let mut command = Command::new("swift");

  command
    .current_dir(swift_package_dir())
    .arg("build")
    .args(["-Xswiftc", "-import-objc-header"])
    .args(["-Xswiftc", header.to_str().expect("a utf-8 path")]);

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
/// SwiftPM records them as the package's own `linkerSettings`, which govern how
/// SwiftPM links — not how cargo does. Linking the static library into a Rust
/// binary leaves every `archive_*` symbol undefined unless we repeat them here.
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

  // The Swift runtime is dynamic — libswift_Concurrency.dylib above all — and
  // the static library records it as `@rpath/...`. Without an rpath the binary
  // links but will not launch, which surfaces as every CLI test failing in dyld
  // rather than as a build error.
  println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
  println!("cargo:rustc-link-lib=framework=Virtualization");
  // For `SecTaskCopyValueForEntitlement`, which is how the Swift side checks
  // that this build was signed before it tries to start a VM.
  println!("cargo:rustc-link-lib=framework=Security");

  for library in SYSTEM_LIBRARIES {
    println!("cargo:rustc-link-lib={library}");
  }
}

fn main() {
  println!("cargo:rerun-if-changed=build.rs");
  println!("cargo:rerun-if-changed=src/lib.rs");

  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }

  println!("cargo:rerun-if-changed={}", swift_source_dir().display());
  println!(
    "cargo:rerun-if-changed={}",
    swift_package_dir().join("Package.swift").display()
  );

  let generated = swift_source_dir().join("generated");

  swift_bridge_build::parse_bridges(vec![manifest_dir().join("src/lib.rs")]).write_all_concatenated(&generated, BRIDGE);
  publish_bridge_shims(&generated);

  compile_swift();

  println!("cargo:rustc-link-lib=static={PACKAGE}");
  println!("cargo:rustc-link-search={}", swift_build_dir().display());

  link_swift_runtime();
}
