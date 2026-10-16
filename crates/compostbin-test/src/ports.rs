use std::net::TcpListener;

pub fn unused_port() -> u16 {
  TcpListener::bind("127.0.0.1:0")
    .expect("bind a host port")
    .local_addr()
    .expect("the bound address")
    .port()
}
