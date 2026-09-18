//! Link arguments for the binary itself.
//!
//! A dependency's `cargo:rustc-link-arg` doesn't reach the binary that links
//! it, so the Swift runtime rpath must come from the executable's package.

fn main() {
  println!("cargo:rerun-if-changed=build.rs");

  if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
    return;
  }

  // The static library references `@rpath/libswift_Concurrency.dylib`; without
  // this the binary links but dyld refuses to launch it.
  println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
