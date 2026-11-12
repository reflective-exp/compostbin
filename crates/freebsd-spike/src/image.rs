use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

const RELEASE: &str = "15.1-RELEASE";
const IMAGE: &str = "FreeBSD-15.1-RELEASE-arm64-aarch64-BASIC-CLOUDINIT-zfs.raw.xz";

pub type Failure = String;

fn run(command: &mut Command) -> Result<String, Failure> {
  let output = command
    .output()
    .map_err(|error| format!("{command:?}: {error}"))?;

  if !output.status.success() {
    return Err(format!(
      "{command:?} exited {}: {}",
      output.status,
      String::from_utf8_lossy(&output.stderr)
    ));
  }

  Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn fetch(url: &str, destination: &Path) -> Result<(), Failure> {
  if destination.exists() {
    return Ok(());
  }

  println!("downloading {url}");
  let partial = destination.with_extension("partial");
  run(
    Command::new("curl")
      .args(["-fsSL", "--retry", "3", "-o"])
      .arg(&partial)
      .arg(url),
  )?;
  std::fs::rename(&partial, destination).map_err(|error| error.to_string())
}

fn sha256(path: &Path) -> Result<String, Failure> {
  let output = run(Command::new("shasum").args(["-a", "256"]).arg(path))?;

  Ok(
    output
      .split_whitespace()
      .next()
      .unwrap_or_default()
      .to_string(),
  )
}

fn verify(path: &Path, expected: &str) -> Result<(), Failure> {
  let actual = sha256(path)?;

  if actual == expected {
    Ok(())
  } else {
    let _ = std::fs::remove_file(path);
    Err(format!("{} has sha256 {actual}, expected {expected}", path.display()))
  }
}

/// The decompressed base disk, downloaded and verified once.
pub fn base_disk(cache: &Path) -> Result<PathBuf, Failure> {
  let raw = cache.join("base.raw");

  if raw.exists() {
    return Ok(raw);
  }

  let directory = format!("https://download.freebsd.org/releases/VM-IMAGES/{RELEASE}/aarch64/Latest");
  let sums = cache.join("CHECKSUM.SHA256");
  let compressed = cache.join(IMAGE);

  fetch(&format!("{directory}/CHECKSUM.SHA256"), &sums)?;
  fetch(&format!("{directory}/{IMAGE}"), &compressed)?;

  let sums = std::fs::read_to_string(&sums).map_err(|error| error.to_string())?;
  let expected = sums
    .lines()
    .find(|line| line.contains(&format!("({IMAGE})")))
    .and_then(|line| line.rsplit(' ').next())
    .ok_or_else(|| format!("CHECKSUM.SHA256 does not list {IMAGE}"))?;
  verify(&compressed, expected)?;

  println!("decompressing {IMAGE}");
  let started = Instant::now();
  let partial = raw.with_extension("partial");
  decompress_sparse(&compressed, &partial)?;
  std::fs::rename(&partial, &raw).map_err(|error| error.to_string())?;
  println!("decompressed in {:.1}s", started.elapsed().as_secs_f64());

  Ok(raw)
}

/// Skips runs of zeros, so the image takes only the space its data needs.
fn decompress_sparse(source: &Path, destination: &Path) -> Result<(), Failure> {
  let input = File::open(source).map_err(|error| error.to_string())?;
  let mut decoder = xz2::read::XzDecoder::new(BufReader::new(input));
  let mut output = File::create(destination).map_err(|error| error.to_string())?;
  let mut chunk = vec![0u8; 1 << 20];
  let mut length = 0u64;

  loop {
    let mut filled = 0;

    while filled < chunk.len() {
      match decoder.read(&mut chunk[filled..]) {
        Ok(0) => break,
        Ok(read) => filled += read,
        Err(error) => return Err(format!("decompressing {}: {error}", source.display())),
      }
    }

    if filled == 0 {
      break;
    }

    let data = &chunk[..filled];
    if data.iter().all(|&byte| byte == 0) {
      output
        .seek(SeekFrom::Current(filled as i64))
        .map_err(|error| error.to_string())?;
    } else {
      output.write_all(data).map_err(|error| error.to_string())?;
    }
    length += filled as u64;
  }

  output.set_len(length).map_err(|error| error.to_string())
}

/// FreeBSD's source, downloaded and verified once.
pub fn source(cache: &Path) -> Result<PathBuf, Failure> {
  let directory = format!("https://download.freebsd.org/releases/arm64/{RELEASE}");
  let manifest = cache.join("MANIFEST");
  let archive = cache.join("src.txz");

  if archive.exists() {
    return Ok(archive);
  }

  fetch(&format!("{directory}/MANIFEST"), &manifest)?;
  fetch(&format!("{directory}/src.txz"), &archive)?;

  let manifest = std::fs::read_to_string(&manifest).map_err(|error| error.to_string())?;
  let expected = manifest
    .lines()
    .find_map(|line| line.strip_prefix("src.txz\t"))
    .and_then(|rest| rest.split('\t').next())
    .ok_or("MANIFEST does not list src.txz")?;
  verify(&archive, expected)?;

  Ok(archive)
}

/// A copy-on-write clone of `base` (APFS `clonefile`, through `cp -c`), grown
/// to `size` bytes for the guest to expand into.
pub fn clone(base: &Path, destination: &Path, size: u64) -> Result<f64, Failure> {
  let _ = std::fs::remove_file(destination);
  let started = Instant::now();

  run(Command::new("cp").arg("-c").arg(base).arg(destination))?;
  let elapsed = started.elapsed().as_secs_f64();

  let file = File::options()
    .write(true)
    .open(destination)
    .map_err(|error| error.to_string())?;
  if file.metadata().map_err(|error| error.to_string())?.len() < size {
    file.set_len(size).map_err(|error| error.to_string())?;
  }

  Ok(elapsed)
}

/// Writes `/efi/freebsd/loader.env` on the disk's EFI system partition:
/// variables FreeBSD's loader sets before `loader.conf`, and passes on to the
/// kernel as tunables. The partition is FAT, which the Mac mounts without root.
pub fn loader_env(disk: &Path, mount_point: &Path, contents: &str) -> Result<(), Failure> {
  let attached = run(
    Command::new("hdiutil")
      .args(["attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage"])
      .arg(disk),
  )?;
  let whole = attached
    .split_whitespace()
    .next()
    .ok_or("hdiutil attached nothing")?
    .to_string();

  let written = (|| {
    let partition = attached
      .lines()
      .find(|line| line.split_whitespace().nth(1) == Some("EFI"))
      .and_then(|line| line.split_whitespace().next())
      .ok_or_else(|| format!("no EFI partition among:\n{attached}"))?;

    std::fs::create_dir_all(mount_point).map_err(|error| error.to_string())?;
    run(
      Command::new("diskutil")
        .args(["mount", "-mountPoint"])
        .arg(mount_point)
        .arg(partition),
    )?;

    let directory = mount_point.join("efi/freebsd");
    let result = std::fs::create_dir_all(&directory)
      .and_then(|()| std::fs::write(directory.join("loader.env"), contents))
      .map_err(|error| format!("writing loader.env: {error}"));

    run(Command::new("diskutil").arg("unmount").arg(mount_point))?;
    result
  })();

  run(Command::new("hdiutil").arg("detach").arg(&whole))?;
  written
}

/// A NoCloud seed volume, labelled `cidata`, holding `files`.
pub fn seed(directory: &Path, iso: &Path, files: &[(&str, &Path)]) -> Result<(), Failure> {
  let _ = std::fs::remove_dir_all(directory);
  let _ = std::fs::remove_file(iso);
  std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;

  for (name, source) in files {
    let target = directory.join(name);

    std::fs::hard_link(source, &target)
      .or_else(|_| std::fs::copy(source, &target).map(|_| ()))
      .map_err(|error| format!("{}: {error}", source.display()))?;
  }

  run(
    Command::new("hdiutil")
      .args([
        "makehybrid",
        "-quiet",
        "-iso",
        "-joliet",
        "-default-volume-name",
        "cidata",
        "-o",
      ])
      .arg(iso)
      .arg(directory),
  )?;

  Ok(())
}
