//! The functions the Swift package exports, as Rust sees them.
//!
//! Its own file because `build.rs` hands this one to swift-bridge's parser,
//! which refuses a `cfg` on the bridge module: the gate is on `mod bridge` in
//! `lib.rs` instead.

#[swift_bridge::bridge]
pub(crate) mod ffi {
  extern "Swift" {
    fn compostbin_last_error() -> String;

    fn compostbin_boot(
      name: &str,
      store_root: &str,
      kernel_path: &str,
      initfs_reference: &str,
      image_reference: &str,
      cpus: i32,
      memory_in_bytes: u64,
      mounts: &str,
      sockets: &str,
      environment: &str,
      arguments: &str,
      working_directory: &str,
      ipv4_address: &str,
      ipv4_gateway: &str,
    ) -> i32;

    fn compostbin_exec(
      name: &str,
      id: &str,
      arguments: &str,
      environment: &str,
      working_directory: &str,
      terminal: i32,
    ) -> i32;

    // `plan` is JSON, alone among these: it nests, and a build step's script may
    // hold the newline the other calls use as a separator. swift-bridge cannot
    // parse a doc comment in here, hence this one.
    fn compostbin_build(plan: &str) -> i32;

    // Also JSON: it carries the kernel's URL and where to put it.
    fn compostbin_provision(spec: &str) -> i32;

    fn compostbin_resize(id: &str, terminal: i32) -> i32;

    fn compostbin_is_running(name: &str) -> bool;

    fn compostbin_is_unpacked(store_root: &str, image_reference: &str) -> i32;
  }
}
