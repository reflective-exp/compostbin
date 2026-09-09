mod pty;

use crate::error::{HostError, PathError, Refusal};
use crate::host::pty::Pty;
use crate::manifest::HostCommand;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// Fixed, not configurable: the guest client is a shell script and cannot read
/// the manifest.
pub const GUEST_SPOOL_TARGET: &str = "/run/compostbin/host";
/// Per project, so sessions cannot read each other's requests.
pub const HOST_SPOOL: &str = "~/.local/state/compostbin/host";

/// Requests land here, one file per request.
pub const REQUESTS_DIR: &str = "requests";
/// Claimed requests are renamed here, so no agent runs one twice.
pub const RUNNING_DIR: &str = "running";
/// Output chunks, and — written last — `<id>.status`.
pub const RESPONSES_DIR: &str = "responses";

/// Submitted as `.partial`, then renamed, so the agent never reads a
/// half-written request.
pub const REQUEST_SUFFIX: &str = ".request";
pub const PARTIAL_SUFFIX: &str = ".partial";

/// The shell's "found but not executable" — the closest existing meaning.
pub const REJECTED_EXIT_CODE: i32 = 126;
/// Killed by a signal. The shell says 128 + signal, but the number is not worth
/// a unix-only import.
pub const SIGNALLED_EXIT_CODE: i32 = 128;

/// Published as numbered chunks — `<id>.out.000001`, `.000002`, … — not one
/// growing file: per F14, a file the guest watches grow stays stale there
/// forever, so every inode it opens must already be complete.
pub const OUTPUT_STREAM: &str = "out";
pub const ERROR_STREAM: &str = "err";
/// Zero-padded, so a lexicographic sort is sequence order — what the client's
/// glob relies on.
pub const SEQUENCE_WIDTH: usize = 6;
/// Big enough that a noisy build does not make thousands of files.
const CHUNK_SIZE: usize = 64 * 1024;
/// A file has no EOF of its own, so the guest marks the end with `<id>.in.eof`.
pub const INPUT_SUFFIX: &str = ".in";
pub const INPUT_EOF_SUFFIX: &str = ".in.eof";
/// Written last, by rename: its appearance means the request is complete.
pub const STATUS_SUFFIX: &str = ".status";

/// Arguments that point a command at other code or configuration. Hygiene
/// against widening one by accident, not a boundary: a command that compiles the
/// repo already runs the repo's code on the host by design.
pub const DENIED_ARGUMENT_PREFIXES: [&str; 3] = ["--config", "--manifest-path", "-Z"];

/// One guest request: the name of an allowlisted command, plus any arguments
/// the guest appended.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
  pub arguments: Vec<String>,
  pub command: String,
}

impl Request {
  pub fn new(command: impl Into<String>, arguments: Vec<String>) -> Self {
    Self {
      arguments,
      command: command.into(),
    }
  }

  /// One field per line, command first: the writer is a shell script, and a
  /// format it emits with `printf '%s\n'` has no escaping to get wrong. The cost
  /// is that an argument may not contain a newline.
  pub fn render(&self) -> Result<String, Refusal> {
    for field in std::iter::once(&self.command).chain(self.arguments.iter()) {
      if field.contains('\n') {
        return Err(Refusal::NewlineInArgument(field.clone()));
      }
    }

    let mut rendered = self.command.clone();
    for argument in &self.arguments {
      rendered.push('\n');
      rendered.push_str(argument);
    }
    rendered.push('\n');

    Ok(rendered)
  }

  pub fn parse(text: &str) -> Result<Self, Refusal> {
    let mut lines = text.lines();
    let command = lines.next().unwrap_or_default().to_string();

    if command.is_empty() {
      return Err(Refusal::EmptyRequest);
    }

    Ok(Self {
      arguments: lines.map(str::to_string).collect(),
      command,
    })
  }
}

/// The argv to run, or why the request is refused. Pure, so the whole allowlist
/// decision is testable without a filesystem or a container.
pub fn resolve(commands: &BTreeMap<String, HostCommand>, request: &Request) -> Result<Vec<String>, Refusal> {
  let Some(command) = commands.get(&request.command) else {
    return Err(Refusal::UnknownCommand(request.command.clone()));
  };

  if command.argv.is_empty() {
    return Err(Refusal::EmptyCommand(request.command.clone()));
  }

  if !request.arguments.is_empty() {
    if !command.arguments {
      return Err(Refusal::ArgumentsNotAllowed(request.command.clone()));
    }

    if let Some(denied) = request
      .arguments
      .iter()
      .find(|argument| is_denied(argument))
    {
      return Err(Refusal::DeniedArgument(denied.clone()));
    }
  }

  let mut argv = command.argv.clone();
  argv.extend(request.arguments.iter().cloned());
  Ok(argv)
}

/// Matches `--config=x` as well as bare `--config`: both reach the same place.
fn is_denied(argument: &str) -> bool {
  DENIED_ARGUMENT_PREFIXES
    .iter()
    .any(|denied| argument == *denied || argument.starts_with(&format!("{denied}=")))
}

/// The request/response directory tree, on whichever side of the mount the
/// caller happens to be.
pub struct Spool {
  root: PathBuf,
}

impl Spool {
  pub fn new(root: impl Into<PathBuf>) -> Self {
    Self { root: root.into() }
  }

  pub fn root(&self) -> &Path {
    &self.root
  }

  pub fn requests(&self) -> PathBuf {
    self.root.join(REQUESTS_DIR)
  }

  pub fn running(&self) -> PathBuf {
    self.root.join(RUNNING_DIR)
  }

  pub fn responses(&self) -> PathBuf {
    self.root.join(RESPONSES_DIR)
  }

  /// Called on the host before the container starts: the guest cannot create
  /// these itself on a mount that does not yet exist.
  pub fn create(&self) -> Result<(), PathError> {
    for directory in [self.requests(), self.running(), self.responses()] {
      std::fs::create_dir_all(&directory).map_err(|source| PathError::new(&directory, source))?;
    }
    Ok(())
  }

  /// Writes a request the way the guest does: `.partial` first, then rename.
  pub fn submit(&self, id: &str, request: &Request) -> Result<(), HostError> {
    let rendered = request.render()?;
    let partial = self.requests().join(format!("{id}{PARTIAL_SUFFIX}"));
    let final_path = self.requests().join(format!("{id}{REQUEST_SUFFIX}"));

    std::fs::write(&partial, rendered).map_err(|source| HostError::Io(PathError::new(&partial, source)))?;
    std::fs::rename(&partial, &final_path).map_err(|source| HostError::Io(PathError::new(&partial, source)))
  }

  /// The oldest unclaimed id. Ids are timestamp-prefixed, so name order is
  /// arrival order.
  fn next_request_id(&self) -> Result<Option<String>, PathError> {
    let directory = self.requests();
    let entries = std::fs::read_dir(&directory).map_err(|source| PathError::new(&directory, source))?;

    let mut ids: Vec<String> = Vec::new();
    for entry in entries {
      let entry = entry.map_err(|source| PathError::new(&directory, source))?;
      let name = entry.file_name().to_string_lossy().into_owned();
      if let Some(id) = name.strip_suffix(REQUEST_SUFFIX) {
        ids.push(id.to_string());
      }
    }

    ids.sort();
    Ok(ids.into_iter().next())
  }
}

/// The rename *is* the claim: it is atomic, so racing agents cannot both win and
/// the loser simply looks again.
fn claim_next(spool: &Spool) -> Result<Option<(String, PathBuf)>, PathError> {
  let Some(id) = spool.next_request_id()? else {
    return Ok(None);
  };

  let submitted = spool.requests().join(format!("{id}{REQUEST_SUFFIX}"));
  let claimed = spool.running().join(&id);

  if std::fs::rename(&submitted, &claimed).is_err() {
    return Ok(None);
  }

  Ok(Some((id, claimed)))
}

/// Runs a claimed request through to its status file and clears the claim.
fn complete(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  id: &str,
  claimed: &Path,
) -> Result<(), PathError> {
  let status = run_claimed(spool, commands, project_dir, id, claimed)?;
  write_status(spool, id, status)?;
  std::fs::remove_file(claimed).map_err(|source| PathError::new(claimed, source))
}

/// Claims and runs one request in the caller's thread, reporting whether there
/// was anything to do. The single-step form the tests drive; sessions use `serve`.
pub fn serve_once(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
) -> Result<bool, PathError> {
  let Some((id, claimed)) = claim_next(spool)? else {
    return Ok(false);
  };

  complete(spool, commands, project_dir, &id, &claimed)?;
  Ok(true)
}

/// Runs one claimed request, returning the exit code to report. Refusals go to
/// the error stream, where the guest would learn of any other failure.
fn run_claimed(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  id: &str,
  claimed: &Path,
) -> Result<i32, PathError> {
  let text = std::fs::read_to_string(claimed).map_err(|source| PathError::new(claimed, source))?;

  let argv = match Request::parse(&text).and_then(|request| resolve(commands, &request).map(|argv| (argv, request))) {
    Ok((argv, request)) => {
      // Looked up separately, so `resolve` stays a decision about what may run
      // rather than how it is wired up.
      let tty = commands
        .get(&request.command)
        .is_some_and(|command| command.tty);
      (argv, tty)
    }
    Err(refusal) => {
      write_refusal(spool, id, &refusal.to_string())?;
      return Ok(REJECTED_EXIT_CODE);
    }
  };

  let (argv, tty) = argv;
  let mut command = Command::new(&argv[0]);
  command.args(&argv[1..]).current_dir(project_dir);

  let started = if tty {
    start_on_terminal(&mut command)
  } else {
    start_on_pipes(&mut command)
  };

  let (mut child, stdin, sources) = match started {
    Ok(started) => started,
    Err(source) => {
      write_refusal(spool, id, &format!("{}: {source}", argv[0]))?;
      return Ok(REJECTED_EXIT_CODE);
    }
  };

  let finished = AtomicBool::new(false);

  // The scope joins every reader, so all chunks are published before this
  // returns and before `complete` writes the status. That is what lets the status
  // file mean "the output is complete".
  let status = std::thread::scope(|scope| {
    scope.spawn(|| {
      if let Some(sink) = stdin {
        pump_stdin(spool, id, sink, &finished);
      }
    });

    for (stream, source) in sources {
      scope.spawn(move || publish_stream(spool, id, stream, source));
    }

    let status = child.wait();
    finished.store(true, Ordering::Relaxed);
    status
  });

  match status {
    Ok(status) => Ok(status.code().unwrap_or(SIGNALLED_EXIT_CODE)),
    Err(source) => Err(PathError::new(&argv[0], source)),
  }
}

/// The child, a sink for its input, and the streams to publish.
type Started = (
  Child,
  Option<Box<dyn Write + Send>>,
  Vec<(&'static str, Box<dyn Read + Send>)>,
);

/// Separate streams, all the way to the guest's own descriptors. The command
/// sees pipes, so `isatty` is false and cargo uses its non-interactive output.
fn start_on_pipes(command: &mut Command) -> io::Result<Started> {
  command
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

  let mut child = command.spawn()?;
  let stdin = child
    .stdin
    .take()
    .map(|sink| Box::new(sink) as Box<dyn Write + Send>);

  let mut sources: Vec<(&'static str, Box<dyn Read + Send>)> = Vec::new();
  if let Some(output) = child.stdout.take() {
    sources.push((OUTPUT_STREAM, Box::new(output)));
  }
  if let Some(errors) = child.stderr.take() {
    sources.push((ERROR_STREAM, Box::new(errors)));
  }

  Ok((child, stdin, sources))
}

/// A real terminal: `isatty` is true, so the command gives colour and progress.
/// A terminal is one device, hence one stream rather than two.
fn start_on_terminal(command: &mut Command) -> io::Result<Started> {
  let terminal = Pty::open()?;
  terminal.attach(command)?;

  let child = command.spawn()?;
  // Dropping our slave is what lets master reads end when the command exits.
  let master = terminal.into_master();
  let reader = File::from(master.try_clone()?);

  Ok((
    child,
    Some(Box::new(File::from(master))),
    vec![(OUTPUT_STREAM, Box::new(reader))],
  ))
}

/// Publishes one stream as numbered chunks, ending at EOF — or at an error,
/// which is how a pty master reports that the command is gone.
fn publish_stream(spool: &Spool, id: &str, stream: &str, mut source: impl Read) {
  let mut buffer = vec![0; CHUNK_SIZE];
  let mut sequence = 0;

  loop {
    match source.read(&mut buffer) {
      Ok(0) | Err(_) => return,
      Ok(read) => {
        sequence += 1;
        // Nothing to be done: the channel back to the guest is what just failed.
        let _ = publish_chunk(spool, id, stream, sequence, &buffer[..read]);
      }
    }
  }
}

/// Written `.partial` then renamed, so the guest's first look at the inode finds
/// it complete (F14).
fn publish_chunk(spool: &Spool, id: &str, stream: &str, sequence: usize, data: &[u8]) -> Result<(), PathError> {
  let name = format!("{id}.{stream}.{sequence:0width$}", width = SEQUENCE_WIDTH);
  let partial = spool.responses().join(format!("{name}{PARTIAL_SUFFIX}"));
  let published = spool.responses().join(&name);

  std::fs::write(&partial, data).map_err(|source| PathError::new(&partial, source))?;
  std::fs::rename(&partial, &published).map_err(|source| PathError::new(&partial, source))
}

/// Feeds the guest's input to the command as it arrives, ending at the guest's
/// `<id>.in.eof` marker. Checking that before reading means the final read sees
/// everything written before it appeared. Dropping `sink` closes the command's
/// stdin, which is what signals EOF to it.
fn pump_stdin(spool: &Spool, id: &str, mut sink: Box<dyn Write + Send>, finished: &AtomicBool) {
  let input_path = spool.responses().join(format!("{id}{INPUT_SUFFIX}"));
  let eof_path = spool.responses().join(format!("{id}{INPUT_EOF_SUFFIX}"));
  let mut offset = 0;

  loop {
    let ended = eof_path.exists() || finished.load(Ordering::Relaxed);

    if let Ok(mut file) = File::open(&input_path) {
      let mut buffer = Vec::new();
      if file.seek(SeekFrom::Start(offset)).is_ok() && file.read_to_end(&mut buffer).is_ok() && !buffer.is_empty() {
        offset += buffer.len() as u64;
        // Normal: the command may simply not read input.
        if sink.write_all(&buffer).is_err() || sink.flush().is_err() {
          return;
        }
      }
    }

    if ended {
      return;
    }

    std::thread::sleep(POLL_INTERVAL);
  }
}

fn write_refusal(spool: &Spool, id: &str, message: &str) -> Result<(), PathError> {
  publish_chunk(
    spool,
    id,
    ERROR_STREAM,
    1,
    format!("compostbin: {message}\n").as_bytes(),
  )
}

/// Written last and by rename, so its appearance unambiguously means "finished,
/// and the output is complete".
fn write_status(spool: &Spool, id: &str, status: i32) -> Result<(), PathError> {
  let partial = spool
    .responses()
    .join(format!("{id}{STATUS_SUFFIX}{PARTIAL_SUFFIX}"));
  let final_path = spool.responses().join(format!("{id}{STATUS_SUFFIX}"));

  std::fs::write(&partial, format!("{status}\n")).map_err(|source| PathError::new(&partial, source))?;
  std::fs::rename(&partial, &final_path).map_err(|source| PathError::new(&partial, source))
}

/// Immediate enough for a command, rare enough to be invisible on the host.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Serves requests until `stop` is set, each on its own thread. Claude's
/// subagents call `compostbin-host` independently, so a long test run must not
/// block everything behind it: requests are claimed one at a time in arrival
/// order, then dispatched. `limit` bounds what is in flight, so a guest looping
/// on submissions cannot spawn unbounded work.
pub fn serve(
  spool: &Spool,
  commands: &BTreeMap<String, HostCommand>,
  project_dir: &Path,
  limit: usize,
  stop: &AtomicBool,
) -> Result<(), PathError> {
  let in_flight = AtomicUsize::new(0);

  std::thread::scope(|scope| {
    while !stop.load(Ordering::Relaxed) {
      if in_flight.load(Ordering::Relaxed) >= limit.max(1) {
        std::thread::sleep(POLL_INTERVAL);
        continue;
      }

      match claim_next(spool)? {
        None => std::thread::sleep(POLL_INTERVAL),
        Some((id, claimed)) => {
          in_flight.fetch_add(1, Ordering::Relaxed);
          let in_flight = &in_flight;

          scope.spawn(move || {
            // One request's failure must not strand every other session command.
            if let Err(error) = complete(spool, commands, project_dir, &id, &claimed) {
              eprintln!("compostbin: serving {id}: {error}");
            }

            in_flight.fetch_sub(1, Ordering::Relaxed);
          });
        }
      }
    }

    Ok(())
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::TempDir;

  fn commands(entries: &[(&str, &[&str], bool)]) -> BTreeMap<String, HostCommand> {
    entries
      .iter()
      .map(|(name, argv, arguments)| {
        (
          name.to_string(),
          HostCommand {
            arguments: *arguments,
            argv: argv.iter().map(|word| word.to_string()).collect(),
            // These tests are all about the pipe path, where the streams stay
            // separate and can be asserted on independently.
            tty: false,
          },
        )
      })
      .collect()
  }

  fn allowlist() -> BTreeMap<String, HostCommand> {
    commands(&[
      ("test", &["cargo", "nextest", "run", "--workspace"], false),
      ("test-one", &["cargo", "nextest", "run"], true),
    ])
  }

  /// A spool with its directories already created, as the host makes it.
  fn spool() -> (TempDir, Spool) {
    let temp = TempDir::new().expect("temp dir");
    let spool = Spool::new(temp.path().canonicalize().expect("canonical temp"));
    spool.create().expect("create spool");
    (temp, spool)
  }

  /// The three things a caller of `compostbin-host` sees: stdout, stderr, status.
  /// A refused request never creates a stdout file, so a missing stream reads as
  /// empty rather than panicking.
  fn serve_one(spool: &Spool, allowlist: &BTreeMap<String, HostCommand>, project_dir: &Path, id: &str) -> Response {
    assert!(
      serve_once(spool, allowlist, project_dir).expect("serve should succeed"),
      "there should have been a request to serve"
    );

    let status = std::fs::read_to_string(spool.responses().join(format!("{id}{STATUS_SUFFIX}"))).unwrap_or_default();

    Response {
      errors: gather(spool, id, ERROR_STREAM),
      output: gather(spool, id, OUTPUT_STREAM),
      status: status.trim().parse().expect("status should be a number"),
    }
  }

  /// Reassembles one stream from its chunks, in sequence order — the same job
  /// the guest client's glob does, and the same ordering assumption.
  fn gather(spool: &Spool, id: &str, stream: &str) -> String {
    let prefix = format!("{id}.{stream}.");
    let mut chunks: Vec<PathBuf> = std::fs::read_dir(spool.responses())
      .expect("responses should be readable")
      .filter_map(|entry| entry.ok().map(|entry| entry.path()))
      .filter(|path| {
        path
          .file_name()
          .and_then(|name| name.to_str())
          .is_some_and(|name| name.starts_with(&prefix) && !name.ends_with(PARTIAL_SUFFIX))
      })
      .collect();

    chunks.sort();
    chunks
      .iter()
      .map(|path| std::fs::read_to_string(path).unwrap_or_default())
      .collect()
  }

  #[derive(Debug, PartialEq)]
  struct Response {
    errors: String,
    output: String,
    status: i32,
  }

  #[test]
  fn resolves_an_allowlisted_command_to_its_manifest_argv() {
    assert_eq!(
      resolve(&allowlist(), &Request::new("test", Vec::new())).expect("should resolve"),
      ["cargo", "nextest", "run", "--workspace"]
    );
  }

  #[test]
  fn refuses_a_command_that_is_not_on_the_allowlist() {
    assert_eq!(
      resolve(&allowlist(), &Request::new("rm", vec!["-rf".to_string()])),
      Err(Refusal::UnknownCommand("rm".to_string()))
    );
  }

  /// The whole point of the per-command flag: `test` is exact, `test-one` is not.
  #[test]
  fn refuses_arguments_unless_the_command_opted_in() {
    let filter = vec!["my_test".to_string()];

    assert_eq!(
      resolve(&allowlist(), &Request::new("test", filter.clone())),
      Err(Refusal::ArgumentsNotAllowed("test".to_string()))
    );
    assert_eq!(
      resolve(&allowlist(), &Request::new("test-one", filter)).expect("should resolve"),
      ["cargo", "nextest", "run", "my_test"]
    );
  }

  #[test]
  fn refuses_arguments_that_redirect_the_command_elsewhere() {
    for argument in ["--config", "--config=target.runner='sh -c'", "--manifest-path", "-Z"] {
      assert_eq!(
        resolve(&allowlist(), &Request::new("test-one", vec![argument.to_string()])),
        Err(Refusal::DeniedArgument(argument.to_string())),
        "{argument} should be refused"
      );
    }
  }

  /// A denied prefix must not swallow a legitimate argument that merely starts
  /// with the same letters.
  #[test]
  fn allows_an_argument_that_only_looks_like_a_denied_one() {
    assert_eq!(
      resolve(
        &allowlist(),
        &Request::new("test-one", vec!["--configured".to_string()])
      )
      .expect("should resolve"),
      ["cargo", "nextest", "run", "--configured"]
    );
  }

  #[test]
  fn round_trips_a_request_through_its_wire_format() {
    let request = Request::new("test-one", vec!["-p".to_string(), "compostbin-core".to_string()]);
    let rendered = request.render().expect("should render");

    assert_eq!(rendered, "test-one\n-p\ncompostbin-core\n");
    assert_eq!(Request::parse(&rendered).expect("should parse"), request);
  }

  /// A newline would parse as a further argument, so it is refused at the writer.
  #[test]
  fn refuses_to_render_an_argument_containing_a_newline() {
    let argument = "one\ntwo".to_string();

    assert_eq!(
      Request::new("test-one", vec![argument.clone()]).render(),
      Err(Refusal::NewlineInArgument(argument))
    );
  }

  #[test]
  fn reports_nothing_to_do_on_an_empty_spool() {
    let (_temp, spool) = spool();

    assert!(!serve_once(&spool, &allowlist(), Path::new(".")).expect("serve should succeed"));
  }

  #[test]
  fn runs_an_allowlisted_command_and_reports_its_output_and_status() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001"),
      Response {
        errors: String::new(),
        output: "hello\n".to_string(),
        status: 0,
      }
    );
  }

  #[test]
  fn reports_the_exit_code_of_a_failing_command() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("fail", &["sh", "-c", "exit 3"], false)]);

    spool
      .submit("0001", &Request::new("fail", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").status, 3);
  }

  /// A refusal must come back through the same channel as output: the guest
  /// only ever sees these two files.
  #[test]
  fn reports_a_refusal_as_output_and_a_status() {
    let (_temp, spool) = spool();

    spool
      .submit("0001", &Request::new("rm", Vec::new()))
      .expect("submit should succeed");

    let response = serve_one(&spool, &allowlist(), Path::new("."), "0001");

    assert_eq!(response.status, REJECTED_EXIT_CODE);
    assert!(
      response.errors.contains("rm"),
      "{} should name the refused command",
      response.errors
    );
  }

  #[test]
  fn runs_the_command_in_the_project_directory() {
    let (_temp, spool) = spool();
    let project = TempDir::new().expect("project dir");
    let project_dir = project.path().canonicalize().expect("canonical project");
    let allowlist = commands(&[("where", &["pwd"], false)]);

    spool
      .submit("0001", &Request::new("where", Vec::new()))
      .expect("submit should succeed");

    assert_eq!(
      serve_one(&spool, &allowlist, &project_dir, "0001")
        .output
        .trim(),
      project_dir.display().to_string()
    );
  }

  /// Ids are timestamp-prefixed, so serving in name order serves in arrival order.
  #[test]
  fn serves_the_oldest_request_first() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[
      ("first", &["echo", "first"], false),
      ("second", &["echo", "second"], false),
    ]);

    spool
      .submit("0002", &Request::new("second", Vec::new()))
      .expect("submit");
    spool
      .submit("0001", &Request::new("first", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "first\n");
    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0002").output, "second\n");
  }

  /// A `.partial` write is invisible until it is renamed, so the agent can never
  /// read a half-written request.
  #[test]
  fn ignores_a_request_that_is_still_being_written() {
    let (_temp, spool) = spool();
    std::fs::write(spool.requests().join(format!("0001{PARTIAL_SUFFIX}")), "greet\n").expect("write partial");

    assert!(!serve_once(&spool, &allowlist(), Path::new(".")).expect("serve should succeed"));
  }

  /// The whole point of splitting the streams: a command writing to both must
  /// not have them merged into one.
  #[test]
  fn keeps_stdout_and_stderr_apart() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("both", &["sh", "-c", "echo out; echo err >&2"], false)]);

    spool
      .submit("0001", &Request::new("both", Vec::new()))
      .expect("submit");

    assert_eq!(
      serve_one(&spool, &allowlist, Path::new("."), "0001"),
      Response {
        errors: "err\n".to_string(),
        output: "out\n".to_string(),
        status: 0,
      }
    );
  }

  /// Input written before the command starts, with the end marked, must reach it.
  #[test]
  fn feeds_the_command_its_input() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("upper", &["tr", "a-z", "A-Z"], false)]);

    std::fs::write(spool.responses().join(format!("0001{INPUT_SUFFIX}")), "hello\n").expect("write input");
    std::fs::write(spool.responses().join(format!("0001{INPUT_EOF_SUFFIX}")), "").expect("mark end");
    spool
      .submit("0001", &Request::new("upper", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "HELLO\n");
  }

  /// A command that never reads stdin, and a guest that never marks the end of
  /// input, must not wedge the agent: the pump stops when the command exits.
  #[test]
  fn finishes_a_command_that_ignores_unterminated_input() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    std::fs::write(spool.responses().join(format!("0001{INPUT_SUFFIX}")), "ignored").expect("write input");
    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");

    assert_eq!(serve_one(&spool, &allowlist, Path::new("."), "0001").output, "hello\n");
  }

  /// The property F14 forced the chunking for: output far larger than one chunk
  /// must come back complete and in order, reassembled from many files.
  #[test]
  fn reassembles_output_spanning_many_chunks() {
    let (_temp, spool) = spool();
    let lines = 50_000;
    let allowlist = commands(&[("many", &["seq", "1", &lines.to_string()], false)]);

    spool
      .submit("0001", &Request::new("many", Vec::new()))
      .expect("submit");
    let response = serve_one(&spool, &allowlist, Path::new("."), "0001");

    let collected: Vec<&str> = response.output.lines().collect();
    assert_eq!(collected.len(), lines, "every line should survive reassembly");
    assert_eq!(collected.first(), Some(&"1"), "and they should be in order");
    assert_eq!(collected.last(), Some(&"50000"));

    let chunks = std::fs::read_dir(spool.responses())
      .expect("responses")
      .filter(|entry| {
        entry.as_ref().is_ok_and(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .contains(&format!(".{OUTPUT_STREAM}."))
        })
      })
      .count();
    assert!(
      chunks > 1,
      "output this size should have taken more than one chunk, got {chunks}"
    );
  }

  /// A chunk is only ever visible once complete, so nothing half-written is left
  /// behind for the guest to read.
  #[test]
  fn leaves_no_partial_chunk_behind() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");
    serve_once(&spool, &allowlist, Path::new(".")).expect("serve should succeed");

    let partials = std::fs::read_dir(spool.responses())
      .expect("responses")
      .filter(|entry| {
        entry.as_ref().is_ok_and(|entry| {
          entry
            .file_name()
            .to_string_lossy()
            .ends_with(PARTIAL_SUFFIX)
        })
      })
      .count();
    assert_eq!(partials, 0);
  }

  #[test]
  fn leaves_no_claimed_request_behind() {
    let (_temp, spool) = spool();
    let allowlist = commands(&[("greet", &["echo", "hello"], false)]);

    spool
      .submit("0001", &Request::new("greet", Vec::new()))
      .expect("submit");
    serve_once(&spool, &allowlist, Path::new(".")).expect("serve should succeed");

    assert_eq!(
      std::fs::read_dir(spool.running())
        .expect("running dir")
        .count(),
      0,
      "the claimed request should have been removed"
    );
  }
}
