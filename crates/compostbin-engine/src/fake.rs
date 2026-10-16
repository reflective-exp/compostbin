use crate::builder::Builder;
use crate::engine::{Engine, Listener};
use crate::error::EngineError;
use crate::model::{BuildPlan, ExecSpec, RunSpec};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

pub const VERSION: &str = "fake engine 1.0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
  Exec(ExecSpec),
  Images,
  IsRunning(String),
  IsUnpacked(String),
  Listen(String, u32),
  MissingContent(String),
  Run(RunSpec),
  Version,
}

#[derive(Debug)]
struct Calls<C> {
  recorded: RefCell<Vec<C>>,
}

impl<C> Default for Calls<C> {
  fn default() -> Self {
    Self {
      recorded: RefCell::default(),
    }
  }
}

impl<C: Clone> Calls<C> {
  fn all(&self) -> Vec<C> {
    self.recorded.borrow().clone()
  }

  fn record(&self, call: C) {
    self.recorded.borrow_mut().push(call);
  }
}

#[derive(Debug, Default)]
pub struct RecordingEngine {
  calls: Calls<Call>,
  listening: RefCell<HashMap<u32, Arc<Doorway>>>,
  running: RefCell<Vec<String>>,
  unpacked: RefCell<Vec<String>>,
  /// `None` makes `images` fail.
  images: Option<Vec<String>>,
  missing: Option<String>,
  exit_code: i32,
}

impl RecordingEngine {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn with_running(containers: &[&str]) -> Self {
    Self {
      running: RefCell::new(containers.iter().copied().map(str::to_string).collect()),
      ..Self::default()
    }
  }

  pub fn with_unpacked(images: &[&str]) -> Self {
    Self {
      unpacked: RefCell::new(images.iter().copied().map(str::to_string).collect()),
      ..Self::default()
    }
  }

  pub fn with_images(images: &[&str]) -> Self {
    Self {
      images: Some(images.iter().copied().map(str::to_string).collect()),
      ..Self::default()
    }
  }

  pub fn missing_content(images: &[&str], digest: &str) -> Self {
    Self {
      missing: Some(digest.to_string()),
      ..Self::with_images(images)
    }
  }

  pub fn exiting_with(code: i32) -> Self {
    Self {
      exit_code: code,
      ..Self::default()
    }
  }

  pub fn calls(&self) -> Vec<Call> {
    self.calls.all()
  }

  /// Connects to `port` as the guest; `ConnectionRefused` when nothing listens there.
  pub fn connect(&self, port: u32) -> io::Result<UnixStream> {
    let refused = || io::Error::from(io::ErrorKind::ConnectionRefused);
    let doorway = self
      .listening
      .borrow()
      .get(&port)
      .cloned()
      .ok_or_else(refused)?;
    let (guest, host) = UnixStream::pair()?;

    match &*lock(&doorway.sender) {
      Some(sender) if sender.send(host.into()).is_ok() => Ok(guest),
      _ => Err(refused()),
    }
  }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Doorway {
  sender: Mutex<Option<Sender<OwnedFd>>>,
}

struct ChannelListener {
  doorway: Arc<Doorway>,
  receiver: Mutex<Receiver<OwnedFd>>,
}

impl Listener for ChannelListener {
  fn accept(&self) -> Option<OwnedFd> {
    lock(&self.receiver).recv().ok()
  }

  fn finish(&self) {
    lock(&self.doorway.sender).take();
  }
}

impl Engine for RecordingEngine {
  fn exec(&self, spec: &ExecSpec) -> Result<i32, EngineError> {
    self.calls.record(Call::Exec(spec.clone()));
    Ok(self.exit_code)
  }

  fn images(&self) -> Result<Vec<String>, EngineError> {
    self.calls.record(Call::Images);

    self
      .images
      .clone()
      .ok_or_else(|| EngineError::unavailable("read the image store", "there is none"))
  }

  fn run(&self, spec: &RunSpec) -> Result<(), EngineError> {
    self.calls.record(Call::Run(spec.clone()));
    self.running.borrow_mut().push(spec.name.clone());
    self.unpacked.borrow_mut().push(spec.image.clone());
    Ok(())
  }

  fn listen(&self, name: &str, port: u32) -> Result<Box<dyn Listener>, EngineError> {
    self.calls.record(Call::Listen(name.to_string(), port));

    let (sender, receiver) = std::sync::mpsc::channel();
    let doorway = Arc::new(Doorway {
      sender: Mutex::new(Some(sender)),
    });
    self
      .listening
      .borrow_mut()
      .insert(port, Arc::clone(&doorway));

    Ok(Box::new(ChannelListener {
      doorway,
      receiver: Mutex::new(receiver),
    }))
  }

  fn is_running(&self, name: &str) -> bool {
    self.calls.record(Call::IsRunning(name.to_string()));

    self.running.borrow().iter().any(|running| running == name)
  }

  fn is_unpacked(&self, image: &str) -> Result<bool, EngineError> {
    self.calls.record(Call::IsUnpacked(image.to_string()));

    Ok(
      self
        .unpacked
        .borrow()
        .iter()
        .any(|unpacked| unpacked == image),
    )
  }

  fn missing_content(&self, image: &str) -> Result<Option<String>, EngineError> {
    self.calls.record(Call::MissingContent(image.to_string()));

    Ok(self.missing.clone())
  }

  fn version(&self) -> String {
    self.calls.record(Call::Version);

    VERSION.to_string()
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BuildCall {
  Build(BuildPlan),
  Provision,
}

#[derive(Debug, Default)]
pub struct RecordingBuilder {
  calls: Calls<BuildCall>,
}

impl RecordingBuilder {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn calls(&self) -> Vec<BuildCall> {
    self.calls.all()
  }

  pub fn plans(&self) -> Vec<BuildPlan> {
    self
      .calls
      .all()
      .into_iter()
      .filter_map(|call| match call {
        BuildCall::Build(plan) => Some(plan),
        BuildCall::Provision => None,
      })
      .collect()
  }
}

impl Builder for RecordingBuilder {
  fn build(&self, plan: &BuildPlan) -> Result<(), EngineError> {
    self.calls.record(BuildCall::Build(plan.clone()));
    Ok(())
  }

  fn provision(&self) -> Result<(), EngineError> {
    self.calls.record(BuildCall::Provision);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::Resources;

  #[test]
  fn records_calls_in_order() {
    let engine = RecordingEngine::new();

    engine.is_running("cb-test");
    engine.version();

    assert_eq!(engine.calls(), [Call::IsRunning("cb-test".to_string()), Call::Version]);
  }

  #[test]
  fn records_nothing_when_unused() {
    assert!(RecordingEngine::new().calls().is_empty());
    assert!(RecordingBuilder::new().plans().is_empty());
  }

  #[test]
  fn keeps_the_plans_it_was_given() {
    let builder = RecordingBuilder::new();
    let plan = BuildPlan::new(
      "docker.io/library/debian:stable-slim",
      "compostbin/base:latest",
      Resources {
        cpus: 4,
        memory_in_bytes: 8 << 30,
      },
    );

    builder.build(&plan).expect("recording should succeed");

    assert_eq!(builder.plans(), [plan]);
  }
}
