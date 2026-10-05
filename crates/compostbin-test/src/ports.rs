//! Host ports, for a test that declares one.

use std::net::TcpListener;

/// A port number nothing is listening on: bound to learn a free one, then
/// released.
pub fn unused_port() -> u16 {
  TcpListener::bind("127.0.0.1:0")
    .expect("bind a host port")
    .local_addr()
    .expect("the bound address")
    .port()
}
