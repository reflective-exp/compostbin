//! The functions the Swift package exports, as Rust sees them.
//!
//! swift-bridge's parser (fed this file by `build.rs`) refuses a `cfg` on the
//! bridge module, so the gate is on `mod bridge` in `lib.rs`.

#[swift_bridge::bridge]
pub(crate) mod ffi {
  extern "Swift" {
    fn czbridge_last_error() -> String;

    fn czbridge_boot(
      name: &str,
      store_root: &str,
      kernel_path: &str,
      initfs_reference: &str,
      image_reference: &str,
      cpus: i32,
      memory_in_bytes: u64,
      rootfs_capacity_in_bytes: u64,
      mounts: &str,
      sockets: &str,
      environment: &str,
      arguments: &str,
      working_directory: &str,
      ipv4_address: &str,
      ipv4_gateway: &str,
    ) -> i32;

    // `term` is empty for a process that gets no terminal, or whose caller
    // leaves `TERM` to the image.
    fn czbridge_exec(
      name: &str,
      id: &str,
      arguments: &str,
      environment: &str,
      user: &str,
      working_directory: &str,
      term: &str,
      terminal: i32,
      stdin: i32,
      stdout: i32,
      stderr: i32,
    ) -> i32;

    // `plan` is JSON: it nests, and a step's script may contain the separators
    // the other calls use. (swift-bridge can't parse doc comments here.)
    fn czbridge_build(plan: &str) -> i32;

    // JSON: the kernel's URL and destination.
    fn czbridge_provision(spec: &str) -> i32;

    fn czbridge_resize(id: &str, terminal: i32) -> i32;

    fn czbridge_is_running(name: &str) -> bool;

    fn czbridge_is_unpacked(store_root: &str, image_reference: &str) -> i32;
  }
}
