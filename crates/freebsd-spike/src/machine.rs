use crate::agent::Agent;
use crate::memory::Memory;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use virtualization::dispatch2::{DispatchQueue, DispatchRetained};
use virtualization::url::Url;
use virtualization::{
  DiskImageStorageDeviceAttachment, EFIBootLoader, EFIVariableStore, EFIVariableStoreInitializationOptions, Error,
  FileHandleSerialPortAttachment, GenericMachineIdentifier, GenericPlatformConfiguration, MACAddress,
  NATNetworkDeviceAttachment, NetworkDeviceConfiguration, SerialPortConfiguration, SharedDirectory,
  SingleDirectoryShare, VirtioBlockDeviceConfiguration, VirtioConsoleDeviceSerialPortConfiguration,
  VirtioFileSystemDeviceConfiguration, VirtioNetworkDeviceConfiguration, VirtualMachine, VirtualMachineConfiguration,
  VirtualMachineState,
};

pub const SHARE_TAG: &str = "share";
/// Reads the kernel's messages out of guest RAM, for a boot that never
/// reaches the agent. Off: suspected of failing save/restore ("I/O error").
const MEMORY_READER: bool = false;

/// Everything a boot of the same guest must find again.
pub struct Layout {
  pub cpus: isize,
  pub disk: PathBuf,
  pub memory: u64,
  pub seed: PathBuf,
  pub share: PathBuf,
  pub variables: PathBuf,
  pub identifier: Vec<u8>,
  pub mac: [u8; 6],
}

pub struct Booted {
  pub agent: Agent,
  pub machine: VirtualMachine,
  pub memory: Memory,
  pub queue: DispatchRetained<DispatchQueue>,
}

fn url(path: &Path) -> Result<Url, String> {
  Url::from_file_path(path).map_err(|()| format!("{} is not absolute", path.display()))
}

fn failed(action: &str) -> impl Fn(Error) -> String + '_ {
  move |error| format!("{action}: {error:?}")
}

impl Layout {
  /// A new machine identifier and EFI variable store for a guest's first boot.
  pub fn first(
    directory: &Path,
    disk: PathBuf,
    seed: PathBuf,
    share: PathBuf,
    cpus: isize,
    memory: u64,
  ) -> Result<Self, String> {
    let variables = directory.join("efi-variables");
    let _ = std::fs::remove_file(&variables);

    EFIVariableStore::with_creating_variable_store_at_and_options(
      &url(&variables)?,
      EFIVariableStoreInitializationOptions::new(0),
    )
    .map_err(failed("create the EFI variable store"))?;

    Ok(Self {
      cpus,
      disk,
      memory,
      seed,
      share,
      variables,
      identifier: GenericMachineIdentifier::new()
        .map_err(failed("make a machine identifier"))?
        .data_representation(),
      mac: MACAddress::random_locally_administered()
        .map_err(failed("make a MAC address"))?
        .ethernet_address(),
    })
  }

  fn configuration(
    &self,
    memory: &Memory,
  ) -> Result<(VirtualMachineConfiguration, std::io::PipeReader, std::io::PipeWriter), String> {
    let configuration = VirtualMachineConfiguration::new().map_err(failed("make a configuration"))?;

    let platform = GenericPlatformConfiguration::new().map_err(failed("make a platform"))?;
    let identifier = GenericMachineIdentifier::with_data_representation(&self.identifier)
      .map_err(failed("read the machine identifier"))?
      .ok_or("the machine identifier did not parse")?;
    platform.set_machine_identifier(&identifier);
    configuration.set_platform(&platform);

    let boot_loader = EFIBootLoader::new().map_err(failed("make the EFI boot loader"))?;
    let variables = EFIVariableStore::new(&url(&self.variables)?).map_err(failed("open the EFI variable store"))?;
    boot_loader.set_variable_store(Some(&variables));
    configuration.set_boot_loader(Some(&boot_loader));
    configuration.set_cpu_count(self.cpus);
    configuration.set_memory_size(self.memory);

    let disk = DiskImageStorageDeviceAttachment::new(&url(&self.disk)?, false).map_err(failed("attach the disk"))?;
    let seed = DiskImageStorageDeviceAttachment::new(&url(&self.seed)?, true).map_err(failed("attach the seed"))?;
    let disk = VirtioBlockDeviceConfiguration::new(&disk).map_err(failed("make the disk device"))?;
    let seed = VirtioBlockDeviceConfiguration::new(&seed).map_err(failed("make the seed device"))?;
    configuration.set_storage_devices(&[&disk, &seed]);

    let network = VirtioNetworkDeviceConfiguration::new().map_err(failed("make the network device"))?;
    network.set_attachment(Some(&NATNetworkDeviceAttachment::new().map_err(failed("make NAT"))?));
    network.set_mac_address(&MACAddress::new(self.mac).map_err(failed("set the MAC address"))?);
    configuration.set_network_devices(&[&network]);

    // The control channel: a single-port virtio console, `/dev/ttyV0.0` in the
    // guest. Not a multiport console device: vtcon(4) attaching one stalls rc
    // under Virtualization, so the guest never reaches networking.
    let (output, guest_output) = std::io::pipe().map_err(|error| error.to_string())?;
    let (guest_input, input) = std::io::pipe().map_err(|error| error.to_string())?;
    let serial = VirtioConsoleDeviceSerialPortConfiguration::new().map_err(failed("make the serial port"))?;
    serial.set_attachment(Some(
      &FileHandleSerialPortAttachment::new(Some(OwnedFd::from(guest_input)), Some(OwnedFd::from(guest_output)))
        .map_err(failed("attach the control port"))?,
    ));
    configuration.set_serial_ports(&[&serial]);

    let share = SharedDirectory::new(&url(&self.share)?, false).map_err(failed("share the directory"))?;
    let file_system =
      VirtioFileSystemDeviceConfiguration::new(SHARE_TAG).map_err(failed("make the virtio-fs device"))?;
    file_system.set_share(Some(
      &SingleDirectoryShare::new(&share).map_err(failed("make the share"))?,
    ));
    configuration.set_directory_sharing_devices(&[&file_system]);

    if MEMORY_READER {
      let memory = memory
        .configuration()
        .map_err(failed("make the memory device"))?;
      configuration.set_custom_virtio_devices(&[&memory]);
    }

    // No graphics device (FreeBSD hangs attaching vtgpu(4)), no entropy device
    // (suspected of stalling rc), and no USB (Apple's xHCI is reported to abort
    // the VM under FreeBSD's xhci(4)).

    configuration
      .validate()
      .map_err(failed("validate the configuration"))?;

    Ok((configuration, output, input))
  }

  /// Whether Virtualization can save and restore this configuration.
  pub fn save_restore_support(&self, queue: &DispatchQueue) -> Result<(), String> {
    let memory = Memory::new();

    on_queue(queue, || {
      let (configuration, _, _) = self.configuration(&memory)?;
      configuration
        .validate_save_restore_support()
        .map_err(failed("validate save/restore support"))
    })
  }

  /// A machine for this guest, not yet started.
  pub fn machine(&self, name: &str) -> Result<Booted, String> {
    let queue = DispatchQueue::new(name, None);
    let memory = Memory::new();
    let (machine, output, input) = on_queue(&queue, || {
      let (configuration, output, input) = self.configuration(&memory)?;
      let machine = VirtualMachine::with_queue(&configuration, &queue).map_err(failed("make the machine"))?;

      Ok::<_, String>((machine, output, input))
    })?;

    Ok(Booted {
      agent: Agent::new(output, input),
      machine,
      memory,
      queue,
    })
  }
}

pub fn on_queue<T: Send>(queue: &DispatchQueue, work: impl FnOnce() -> T + Send) -> T {
  let mut result = None;

  queue.exec_sync(|| result = Some(work()));

  result.expect("the queue ran the work")
}

type Done = Box<dyn FnOnce(Result<(), Error>) + Send>;

impl Booted {
  /// Calls `operation` on the machine's queue with a completion handler, and
  /// waits for it.
  fn completed(
    &self,
    action: &str,
    operation: impl FnOnce(&VirtualMachine, Done) -> Result<(), Error> + Send,
  ) -> Result<f64, String> {
    let (sender, receiver) = mpsc::channel();
    let started = Instant::now();
    let machine = &self.machine;

    on_queue(&self.queue, || {
      operation(
        machine,
        Box::new(move |result| {
          let _ = sender.send(result);
        }),
      )
    })
    .map_err(failed(action))?;

    receiver
      .recv()
      .map_err(|error| format!("{action}: {error}"))?
      .map_err(failed(action))?;

    Ok(started.elapsed().as_secs_f64())
  }

  pub fn start(&self) -> Result<f64, String> {
    self.completed("start", |machine, done| {
      machine.start(done);
      Ok(())
    })
  }

  pub fn pause(&self) -> Result<f64, String> {
    self.completed("pause", |machine, done| {
      machine.pause(done);
      Ok(())
    })
  }

  pub fn resume(&self) -> Result<f64, String> {
    self.completed("resume", |machine, done| {
      machine.resume(done);
      Ok(())
    })
  }

  pub fn stop(&self) -> Result<f64, String> {
    self.completed("stop", |machine, done| {
      machine.stop(done);
      Ok(())
    })
  }

  pub fn save(&self, path: &Path) -> Result<f64, String> {
    let url = url(path)?;

    self.completed("save the machine's state", |machine, done| {
      machine.save_machine_state_to(&url, done)
    })
  }

  pub fn restore(&self, path: &Path) -> Result<f64, String> {
    let url = url(path)?;

    self.completed("restore the machine's state", |machine, done| {
      machine.restore_machine_state_from(&url, done)
    })
  }

  pub fn state(&self) -> VirtualMachineState {
    let machine = &self.machine;

    on_queue(&self.queue, || machine.state())
  }

  /// Waits for the guest to power itself off.
  pub fn wait_stopped(&self, timeout: Duration) -> Result<f64, String> {
    let started = Instant::now();

    while started.elapsed() < timeout {
      match self.state() {
        VirtualMachineState::Stopped => return Ok(started.elapsed().as_secs_f64()),
        VirtualMachineState::Error => return Err("the machine stopped with an error".to_string()),
        _ => std::thread::sleep(Duration::from_millis(100)),
      }
    }

    Err(format!("the guest did not stop within {}s", timeout.as_secs()))
  }
}
