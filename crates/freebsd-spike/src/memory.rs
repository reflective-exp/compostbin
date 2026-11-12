//! Reads the FreeBSD kernel's message buffer straight out of guest RAM, for a
//! kernel that has no console Virtualization can show.
//!
//! Guest memory is reachable only through a custom virtio device.

use crate::machine::on_queue;
use std::sync::{Arc, Mutex};
use virtualization::dispatch2::{DispatchQueue, DispatchRetained};
use virtualization::{
  CustomVirtioDevice, CustomVirtioDeviceConfiguration, CustomVirtioDeviceConfigurationDelegate,
  CustomVirtioDeviceDelegateProvider, Error,
};

/// The watchdog device: FreeBSD has no driver for it, so the guest never
/// touches it.
const DEVICE_ID: u16 = 21;
/// `MSG_MAGIC` in `sys/sys/msgbuf.h`.
const MSG_MAGIC: u32 = 0x063062;
const CHUNK: u64 = 64 << 20;
/// Where to look for RAM: Virtualization doesn't say where it puts it.
const PROBE_LIMIT: u64 = 64 << 30;
const PROBE_STEP: u64 = 16 << 20;

#[derive(Default)]
struct Created {
  device: Mutex<Option<CustomVirtioDevice>>,
}

impl CustomVirtioDeviceConfigurationDelegate for Created {
  fn custom_virtio_configuration_did_create_device(
    &self,
    _configuration: &CustomVirtioDeviceConfiguration,
    device: &CustomVirtioDevice,
  ) {
    *self
      .device
      .lock()
      .unwrap_or_else(|poison| poison.into_inner()) = Some(device.clone());
  }
}

/// The device a machine is configured with, and the delegate Virtualization
/// holds only weakly.
pub struct Memory {
  created: Arc<Created>,
  queue: DispatchRetained<DispatchQueue>,
}

impl Memory {
  pub fn new() -> Self {
    Self {
      created: Arc::default(),
      queue: DispatchQueue::new("spike-memory", None),
    }
  }

  pub fn configuration(&self) -> Result<CustomVirtioDeviceConfiguration, Error> {
    let delegate: Arc<dyn CustomVirtioDeviceConfigurationDelegate> = self.created.clone();
    let configuration = CustomVirtioDeviceConfiguration::new()?;

    configuration.set_device_id(DEVICE_ID);
    configuration.set_pci_class_id(0xff);
    configuration.set_virtio_queue_count(1);
    configuration.set_supports_save_restore(true);
    configuration.set_provider(Some(&CustomVirtioDeviceDelegateProvider::new(&self.queue, &delegate)?));

    Ok(configuration)
  }

  /// The kernel's messages so far, or why they couldn't be read.
  pub fn kernel_messages(&self) -> Result<String, String> {
    let device = self
      .created
      .device
      .lock()
      .unwrap_or_else(|poison| poison.into_inner())
      .clone()
      .ok_or("Virtualization never created the memory device")?;

    on_queue(&self.queue, || read(&device))
  }
}

/// Copies `length` bytes of guest memory at `address`, if it is RAM.
fn copy(device: &CustomVirtioDevice, address: u64, length: u64) -> Option<Vec<u8>> {
  let mapping = device.guest_memory_mapping(address, isize::try_from(length).ok()?)?;
  let mapped = usize::try_from(mapping.length()).ok()?;
  let mut bytes = vec![0u8; mapped];

  // SAFETY: Virtualization maps `mapped` bytes at `mutable_bytes` for as long
  // as `mapping` lives. The guest may write them meanwhile; a torn read only
  // garbles text we print.
  unsafe {
    std::ptr::copy_nonoverlapping(mapping.mutable_bytes().cast::<u8>(), bytes.as_mut_ptr(), mapped);
  }

  Some(bytes)
}

fn word(bytes: &[u8], at: usize) -> Option<u32> {
  Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Finds RAM, then `struct msgbuf` by its magic: `msg_ptr` (8 bytes),
/// `msg_magic`, `msg_size`, `msg_wseq`, with the buffer's `msg_size` bytes
/// just before the struct (`msgbufinit`).
fn read(device: &CustomVirtioDevice) -> Result<String, String> {
  let mut ram = Vec::new();
  let mut address = 0;

  while address < PROBE_LIMIT {
    if device.guest_memory_mapping(address, 4096).is_some() {
      ram.push(address);
    }
    address += PROBE_STEP;
  }

  let (Some(&first), Some(&last)) = (ram.first(), ram.last()) else {
    return Err("no guest RAM below 64 GiB".to_string());
  };
  let end = last + PROBE_STEP;
  let mut found = Vec::new();
  let mut chunk = first;

  while chunk < end {
    if let Some(bytes) = copy(device, chunk, CHUNK.min(end - chunk)) {
      for at in (8..bytes.len().saturating_sub(16)).step_by(8) {
        if word(&bytes, at) != Some(MSG_MAGIC) {
          continue;
        }
        let (Some(size), Some(sequence)) = (word(&bytes, at + 4), word(&bytes, at + 8)) else {
          continue;
        };
        if !(4096..=(16 << 20)).contains(&size) {
          continue;
        }

        let header = chunk + (at - 8) as u64;
        if let Some(buffer) = copy(device, header - u64::from(size), u64::from(size)) {
          found.push(text(&buffer, size, sequence, header));
        }
      }
    }
    chunk += CHUNK;
  }

  if found.is_empty() {
    Err(format!(
      "RAM spans {first:#x}..{end:#x}, but holds no kernel message buffer: the kernel never initialized one"
    ))
  } else {
    Ok(found.join("\n\n"))
  }
}

/// The ring's contents in order: `msg_wseq` counts bytes written, modulo a
/// multiple of the size.
fn text(buffer: &[u8], size: u32, sequence: u32, header: u64) -> String {
  let position = (sequence % size) as usize;
  let ordered: Vec<u8> = if sequence < size {
    buffer[..position].to_vec()
  } else {
    [&buffer[position..], &buffer[..position]].concat()
  };

  format!(
    "message buffer at {header:#x} ({} of {size} bytes):\n{}",
    ordered.len(),
    String::from_utf8_lossy(&ordered).trim_matches('\0')
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reads_a_buffer_that_has_not_wrapped_up_to_where_it_was_written() {
    let mut buffer = b"---<<BOOT>>---\nCopyright".to_vec();
    buffer.resize(4096, 0);

    assert!(text(&buffer, 4096, 15, 0x1000).ends_with(":\n---<<BOOT>>---\n"));
  }

  #[test]
  fn reads_a_wrapped_buffer_from_its_oldest_byte() {
    let mut buffer = vec![b'x'; 4096];
    buffer[..3].copy_from_slice(b"new");
    buffer[3..6].copy_from_slice(b"old");

    let read = text(&buffer, 4096, 4096 * 2 + 3, 0x1000);

    assert!(read.contains(":\nold"), "{read}");
    assert!(read.ends_with("new"), "{read}");
  }
}
