pub(crate) fn encode(bytes: &[u8]) -> String {
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn encodes_each_byte_as_two_lowercase_digits() {
    assert_eq!(encode(&[0x00, 0x0f, 0xab]), "000fab");
  }
}
