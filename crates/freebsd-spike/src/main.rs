//! Boots FreeBSD's arm64 cloud-init VM image on Virtualization.framework
//! through UEFI, from a copy-on-write clone of a cached base disk, and
//! measures it: first and cold boot, a host-guest control channel over a
//! virtio console port, virtio-fs sharing through the patched modules in
//! `guest/virtiofs-15.1.patch`, and save/restore.
//!
//! `freebsd-spike <work directory>`. Run it signed: `tests/freebsd.rs`.

mod agent;
mod image;
mod lease;
mod machine;
mod memory;

use machine::{Booted, Layout, SHARE_TAG};
use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DISK_SIZE: u64 = 24 << 30;
const MEMORY: u64 = 4 << 30;
const CPUS: isize = 4;
const HELLO: &str = "hello from the host";
/// Verbose boot logs the virtio features offered and negotiated. vtnet(4)
/// fails to attach under Virtualization ("desired features were not
/// accepted"); these drop the offloads and multiqueue from what it asks for.
const LOADER_ENV: &str = "boot_verbose=YES\nhw.vtnet.csum_disable=1\nhw.vtnet.mq_disable=1\n";

#[derive(Default)]
struct Report {
  rows: Vec<(String, String)>,
  notes: Vec<String>,
}

impl Report {
  fn row(&mut self, name: &str, value: impl Into<String>) {
    let value = value.into();

    println!("== {name}: {value}");
    self.rows.push((name.to_string(), value));
  }

  fn seconds(&mut self, name: &str, seconds: f64) {
    self.row(name, format!("{seconds:.2}s"));
  }

  fn note(&mut self, note: impl Into<String>) {
    let note = note.into();

    println!("== note: {note}");
    self.notes.push(note);
  }

  fn markdown(&self) -> String {
    let mut text = String::from("# FreeBSD spike\n\n| step | result |\n| --- | --- |\n");

    for (name, value) in &self.rows {
      let _ = writeln!(text, "| {name} | {} |", value.replace('|', "\\|").replace('\n', "<br>"));
    }

    if !self.notes.is_empty() {
      text.push_str("\n## Notes\n\n");
      for note in &self.notes {
        let _ = writeln!(text, "~~~\n{note}\n~~~\n");
      }
    }

    text
  }
}

struct Run {
  cache: PathBuf,
  directory: PathBuf,
  report: Report,
}

fn main() {
  let work = PathBuf::from(
    std::env::args()
      .nth(1)
      .expect("usage: freebsd-spike <work directory>"),
  );
  let mut run = Run {
    cache: work.join("cache"),
    directory: work.join("run"),
    report: Report::default(),
  };

  std::fs::create_dir_all(&run.cache).expect("make the cache directory");
  let _ = std::fs::remove_dir_all(&run.directory);
  std::fs::create_dir_all(&run.directory).expect("make the run directory");

  let outcome = run.all();

  if let Err(error) = &outcome {
    run.report.row("FAILED", error.clone());
  }

  let report = run.report.markdown();
  std::fs::write(work.join("report.md"), &report).expect("write the report");
  println!("\n{report}");
  println!("report: {}", work.join("report.md").display());

  std::process::exit(i32::from(outcome.is_err()));
}

fn guest_lines(ran: &agent::Ran) -> String {
  ran.output.join("\n")
}

impl Run {
  fn all(&mut self) -> Result<(), String> {
    self.links_both_bridges()?;

    let base = image::base_disk(&self.cache)?;
    let source = image::source(&self.cache)?;
    let disk = self.directory.join("disk.raw");
    let seconds = image::clone(&base, &disk, DISK_SIZE)?;
    self.report.seconds("clone the base disk (cp -c)", seconds);
    image::loader_env(&disk, &self.directory.join("esp"), LOADER_ENV)?;
    self
      .report
      .row("loader.env", LOADER_ENV.trim().replace('\n', "; "));

    let share = self.directory.join("share");
    std::fs::create_dir_all(&share).map_err(|error| error.to_string())?;
    std::fs::write(share.join("hello.txt"), HELLO).map_err(|error| error.to_string())?;

    let seed = self.seed(&source)?;
    let layout = Layout::first(&self.directory, disk, seed, share.clone(), CPUS, MEMORY)?;

    let support_queue = virtualization::dispatch2::DispatchQueue::new("spike-validate", None);
    let save_restore = layout.save_restore_support(&support_queue);
    self.report.row(
      "save/restore supported by this configuration",
      match &save_restore {
        Ok(()) => "yes".to_string(),
        Err(error) => format!("no: {error}"),
      },
    );

    let mut first = layout.machine("spike-first")?;
    self.boot(
      &mut first,
      layout.mac,
      "first boot (cloud-init)",
      Duration::from_secs(600),
    )?;
    // rc deletes /firstboot only when it finishes, after rc.local starts the
    // agent, and reboots then if the first boot's package upgrade installed
    // anything. Powered off sooner, the next boot is a first boot again.
    let started = Instant::now();
    first
      .agent
      .send("while [ -e /firstboot ]; do sleep 1; done; echo firstboot-done")?;
    let finished = first
      .agent
      .wait_for_any(&["firstboot-done", "spike-ready"], Duration::from_secs(900))?
      .ok_or("the first boot's rc did not finish within 900s")?;
    self.report.row(
      "first boot's rc finished",
      format!(
        "{:.1}s after the agent answered{}",
        started.elapsed().as_secs_f64(),
        if finished.starts_with("spike-ready") {
          ", by rebooting (package upgrade)"
        } else {
          ""
        }
      ),
    );
    self.describe(&mut first)?;
    self.power_off(&mut first)?;
    drop(first);

    let mut cold = layout.machine("spike-cold")?;
    self.boot(&mut cold, layout.mac, "cold boot", Duration::from_secs(300))?;
    if let Err(error) = self.virtiofs(&mut cold, &share) {
      let state = cold
        .agent
        .run(
          "uptime; dmesg | tail -40; ps -axo pid,ppid,stat,etime,command | tail -40",
          Duration::from_secs(30),
        )
        .map_or_else(|error| format!("unreadable: {error}"), |ran| guest_lines(&ran));
      self
        .report
        .note(format!("the guest when the virtio-fs steps failed:\n{state}"));
      return Err(error);
    }

    if save_restore.is_ok() {
      let mut restored = self.save_and_restore(&layout, cold)?;
      self.power_off(&mut restored)?;
    } else {
      self.power_off(&mut cold)?;
    }

    Ok(())
  }

  /// Both swift-bridge crates in one binary: this links only if their symbols don't clash.
  fn links_both_bridges(&mut self) -> Result<(), String> {
    use compostbin_engine::images::Images;

    let empty = self.directory.join("empty-store");
    std::fs::create_dir_all(&empty).map_err(|error| error.to_string())?;
    let references = compostbin_engine::containerization::Store::at(&empty).references();

    self.report.row(
      "containerization-framework and virtualization in one binary",
      format!("links and runs; an empty Containerization store lists {references:?}"),
    );
    Ok(())
  }

  fn seed(&mut self, source: &Path) -> Result<PathBuf, String> {
    let guest = Path::new(env!("CARGO_MANIFEST_DIR")).join("guest");
    let meta = self.directory.join("meta-data");
    let instance = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |since| since.as_secs());
    std::fs::write(&meta, format!("instance-id: spike-{instance}\nlocal-hostname: spike\n"))
      .map_err(|error| error.to_string())?;

    let iso = self.directory.join("seed.iso");
    let started = Instant::now();
    image::seed(
      &self.directory.join("seed"),
      &iso,
      &[
        ("meta-data", &meta),
        ("user-data", &guest.join("user-data.sh")),
        ("src.txz", source),
        ("virtiofs-15.1.patch", &guest.join("virtiofs-15.1.patch")),
      ],
    )?;
    self
      .report
      .seconds("make the seed volume", started.elapsed().as_secs_f64());

    Ok(iso)
  }

  /// Waits for the agent, watching meanwhile for the guest's DHCP lease and
  /// its ssh port: how far a guest that never answers got.
  fn boot(&mut self, booted: &mut Booted, mac: [u8; 6], name: &str, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    let mut leased: Option<(Ipv4Addr, f64)> = None;
    let mut ssh: Option<f64> = None;
    let mut dumps = vec![90.0, 240.0, 420.0];
    booted.start()?;

    let line = loop {
      if let Some(line) = booted
        .agent
        .wait_for("spike-ready", Duration::from_secs(2))?
      {
        break Some(line);
      }

      let elapsed = started.elapsed().as_secs_f64();
      if leased.is_none()
        && let Some(address) = lease::lease(mac)
      {
        println!("== {name}: DHCP lease {address} at {elapsed:.1}s");
        leased = Some((address, elapsed));
      }
      if let Some((address, _)) = leased
        && ssh.is_none()
        && TcpStream::connect_timeout(&SocketAddr::from((address, 22)), Duration::from_millis(500)).is_ok()
      {
        println!("== {name}: ssh answering at {elapsed:.1}s");
        ssh = Some(elapsed);
      }

      if let Some(&at) = dumps.first()
        && elapsed > at
      {
        dumps.remove(0);
        self.kernel_messages(booted, &format!("{name}: kernel messages at {at:.0}s, no agent yet"));
      }

      if started.elapsed() > timeout {
        self.kernel_messages(booted, &format!("{name}: kernel messages at the timeout"));
        break None;
      }
    };

    let progress = format!(
      "DHCP lease: {}; ssh: {}",
      leased.map_or("none".to_string(), |(address, at)| format!("{address} at {at:.1}s")),
      ssh.map_or("never answered".to_string(), |at| format!("answering at {at:.1}s")),
    );

    let Some(line) = line else {
      return Err(format!("{name}: no agent within {}s; {progress}", timeout.as_secs()));
    };

    let total = started.elapsed().as_secs_f64();
    let since_kernel = line.split_whitespace().nth(1).unwrap_or("?");
    self.report.row(
      name,
      format!(
        "{total:.2}s from start to the agent; the guest says {since_kernel}s since its kernel booted; {progress}"
      ),
    );
    Ok(())
  }

  fn kernel_messages(&mut self, booted: &Booted, heading: &str) {
    let messages = booted
      .memory
      .kernel_messages()
      .unwrap_or_else(|error| format!("unreadable: {error}"));

    self.report.note(format!("{heading}\n{messages}"));
  }

  fn describe(&mut self, booted: &mut Booted) -> Result<(), String> {
    let ran = booted.agent.check(
      "uname -a; freebsd-version -ku; sysctl -n hw.ncpu hw.physmem; ls /dev/ttyV*; zpool list -H -o name,size; \
       pciconf -l | grep -c virtio; kldstat | grep -c .; echo \"agent starts in rc.local: $(grep -c spike-agent \
       /etc/rc.local)\"; echo \"agent shells: $(pgrep -f 'spike-agent' | wc -l)\"",
      Duration::from_secs(30),
    )?;
    self.report.row("guest", guest_lines(&ran));

    let ran = booted.agent.run(
      "dmesg | grep -i -E 'virtio|vtcon|vtnet|vtblk|vtfs|unknown' | head -40",
      Duration::from_secs(30),
    )?;
    self
      .report
      .note(format!("virtio devices in dmesg:\n{}", guest_lines(&ran)));
    Ok(())
  }

  fn power_off(&mut self, booted: &mut Booted) -> Result<(), String> {
    booted.agent.send("shutdown -p now")?;
    let seconds = booted.wait_stopped(Duration::from_secs(120))?;
    self.report.seconds("power off", seconds);
    Ok(())
  }

  fn virtiofs(&mut self, booted: &mut Booted, share: &Path) -> Result<(), String> {
    let agent = &mut booted.agent;
    let long = Duration::from_secs(1800);

    agent.check(
      "mkdir -p /mnt/seed && mount -t cd9660 \"$(ls /dev/iso9660/* | head -1)\" /mnt/seed && ls /mnt/seed",
      Duration::from_secs(30),
    )?;

    let ran = agent.check("tar -xf /mnt/seed/src.txz -C / && find /usr/src | wc -l", long)?;
    self.report.row(
      "extract src.txz to the guest's own disk (ZFS)",
      format!("{:.1}s, {} entries", ran.seconds, guest_lines(&ran).trim()),
    );

    agent.check(
      "cd /usr/src && patch -s -p1 -i /mnt/seed/virtiofs-15.1.patch",
      Duration::from_secs(60),
    )?;
    // Ours: the patch's module Makefile omits the generated vnode_if.h, which
    // fusefs's lists first.
    agent.check(
      "sed -i '' 's/^SRCS=\\([[:space:]]*\\)virtiofs_vfsops.c/SRCS=\\1vnode_if.h virtiofs_vfsops.c/' \
       /usr/src/sys/modules/virtiofs/Makefile && grep ^SRCS /usr/src/sys/modules/virtiofs/Makefile",
      Duration::from_secs(30),
    )?;
    self
      .report
      .row("fix the virtiofs module Makefile", "add vnode_if.h to SRCS");

    // A subshell, so a failure ends the step rather than the agent's shell.
    // `all` apart from `install`: config.mk skips finding the `_if.m` files
    // whenever `install` is a target, so `make all install` cannot build vtfs.
    let built = agent.run(
      "(for m in fusefs virtio/fs virtiofs; do make -s -C /usr/src/sys/modules/$m all && make -s -C \
       /usr/src/sys/modules/$m install || exit 1; done)",
      long,
    )?;
    if built.code != 0 {
      let probe = agent.run(
        "cd /usr/src/sys/modules/virtio/fs && ls -la . /usr/src/sys/dev/virtio/fs; make -V SYSDIR -V .OBJDIR; make -V \
         __MPATH | tr ' ' '\\n' | grep -c _if.m; make -V __MPATH | tr ' ' '\\n' | grep virtio; make -n all 2>&1 | head \
         -20; echo control:; make -s -C /usr/src/sys/modules/virtio/random 2>&1 | tail -5; echo control exited $?",
        long,
      )?;
      self.report.note(format!(
        "building the modules failed:\n{}\nwhat make sees in sys/modules/virtio/fs:\n{}",
        guest_lines(&built),
        guest_lines(&probe)
      ));
      return Err(format!("building the modules exited {}", built.code));
    }
    let ran = built;
    self
      .report
      .seconds("build fusefs, vtfs, virtiofs modules in the guest", ran.seconds);

    let ran = agent.check(
      "kldunload fusefs 2>/dev/null; kldload /boot/modules/fusefs.ko && kldload /boot/modules/vtfs.ko && kldload \
       /boot/modules/virtiofs.ko && kldstat | grep -E 'fusefs|vtfs|virtiofs'",
      Duration::from_secs(60),
    )?;
    self.report.row("load the modules", guest_lines(&ran));

    let ran = agent.run(
      &format!("mkdir -p /mnt/share && mount -t virtiofs {SHARE_TAG} /mnt/share && mount | grep share"),
      Duration::from_secs(60),
    )?;
    let dmesg = agent.run("dmesg | tail -15", Duration::from_secs(30))?;
    self
      .report
      .note(format!("dmesg after mounting:\n{}", guest_lines(&dmesg)));
    if ran.code != 0 {
      return Err(format!(
        "mount -t virtiofs failed ({}): {}",
        ran.code,
        guest_lines(&ran)
      ));
    }
    self.report.row("mount -t virtiofs", guest_lines(&ran));

    let ran = agent.check("cat /mnt/share/hello.txt", Duration::from_secs(30))?;
    let read = guest_lines(&ran);
    self.report.row(
      "guest reads a host file",
      if read.trim() == HELLO {
        "ok".to_string()
      } else {
        format!("read {read:?}")
      },
    );

    agent.check(
      "echo from-guest > /mnt/share/from-guest.txt && sync",
      Duration::from_secs(30),
    )?;
    let written = std::fs::read_to_string(share.join("from-guest.txt")).unwrap_or_default();
    self.report.row(
      "host reads a guest file",
      if written.trim() == "from-guest" {
        "ok".to_string()
      } else {
        format!("read {written:?}")
      },
    );

    for (name, directory) in [("virtio-fs", "/mnt/share"), ("guest disk", "/var/tmp")] {
      let ran = agent.check(
        &format!(
          "echo \"write: $(dd if=/dev/zero of={directory}/big bs=1m count=1024 2>&1 | tail -1)\"; echo \"read: $(dd \
           if={directory}/big of=/dev/null bs=1m 2>&1 | tail -1)\"; rm {directory}/big"
        ),
        long,
      )?;
      self
        .report
        .row(&format!("1 GiB write then read, {name}"), guest_lines(&ran));
    }

    let ran = agent.check(
      "tar -xf /mnt/seed/src.txz -C /mnt/share && find /mnt/share/usr/src | wc -l",
      long,
    )?;
    self.report.row(
      "extract src.txz onto virtio-fs",
      format!("{:.1}s, {} entries", ran.seconds, guest_lines(&ran).trim()),
    );
    self.report.row(
      "host sees the extracted tree",
      if share.join("usr/src/Makefile").exists() {
        "yes"
      } else {
        "no"
      },
    );

    for (name, directory) in [("guest disk", "/usr/src"), ("virtio-fs", "/mnt/share/usr/src")] {
      let ran = agent.check(
        &format!("make -s -C {directory}/sys/modules/fusefs clean all >/dev/null"),
        long,
      )?;
      self
        .report
        .seconds(&format!("build fusefs.ko from a tree on the {name}"), ran.seconds);
    }

    let ran = agent.check("rm -rf /mnt/share/usr", long)?;
    self
      .report
      .seconds("delete the extracted tree on virtio-fs", ran.seconds);

    agent.check("echo alive", Duration::from_secs(30))?;
    self
      .report
      .row("guest still answering after the workload", "yes");
    Ok(())
  }

  fn save_and_restore(&mut self, layout: &Layout, booted: Booted) -> Result<Booted, String> {
    let state = self.directory.join("state.vzvmsave");

    let pause = booted.pause()?;
    let save = booted.save(&state)?;
    booted.stop()?;
    drop(booted);
    let size = std::fs::metadata(&state).map_or(0, |metadata| metadata.len());
    self.report.row(
      "pause and save",
      format!("{pause:.2}s pausing, {save:.2}s saving, {} MiB of state", size >> 20),
    );

    let mut restored = layout.machine("spike-restored")?;
    let started = Instant::now();
    let restore = restored.restore(&state)?;
    let resume = restored.resume()?;
    let ran = restored
      .agent
      .check("echo restored; ls /mnt/share", Duration::from_secs(60))?;
    self.report.row(
      "restore and resume",
      format!(
        "{restore:.2}s restoring, {resume:.2}s resuming, {:.2}s until the agent answered; /mnt/share lists: {}",
        started.elapsed().as_secs_f64(),
        ran
          .output
          .iter()
          .skip(1)
          .cloned()
          .collect::<Vec<_>>()
          .join(" ")
      ),
    );

    Ok(restored)
  }
}
