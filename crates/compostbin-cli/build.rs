//! Link arguments for the binary itself.
//!
//! `containerization-framework-bridge` carries the Swift static library, but a
//! dependency's `cargo:rustc-link-arg` applies only to that dependency's own
//! link step — it does not reach the binary linking it. The rpath has to be
//! emitted here, in the package that produces the executable.

fn main() {
  println!("cargo:rerun-if-changed=build.rs");

  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }

  // The Swift runtime is dynamic, and the static library records it as
  // `@rpath/libswift_Concurrency.dylib`. Without this the binary links but
  // dyld refuses to launch it.
  println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
