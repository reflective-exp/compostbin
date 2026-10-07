//! The OCI documents an image is made of: reading an image's config, and
//! writing a built rootfs into the store as an image of one layer.

use crate::error::EngineError;
use containerization_framework::containerization::image::Image;
use containerization_framework::containerization_oci::content::{ContentWriter, LocalContentStore};
use containerization_framework::containerization_oci::image::{Descriptor, Platform};
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

const INDEX: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_INDEX: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG: &str = "application/vnd.oci.image.config.v1+json";
/// Uncompressed: nothing pushes these images, and an uncompressed blob's digest
/// is also its diffID, so both are right in one pass.
const LAYER: &str = "application/vnd.oci.image.layer.v1.tar";

/// What an image's config says a process runs as: `User`, `Env`, `Entrypoint`,
/// `Cmd`, `WorkingDir` and `Labels`. Empty for an image that says nothing.
pub type ImageConfig = Map<String, Value>;

/// The config of `image`'s manifest for `platform`.
pub fn image_config(image: &Image, platform: &Platform) -> Result<ImageConfig, EngineError> {
  let read = |digest: &str| -> Result<Value, EngineError> {
    let data = image.get_content(digest)?.data()?;

    serde_json::from_slice(&data).map_err(|error| EngineError::failed(format!("read {digest}"), error))
  };

  let mut document = read(&image.digest())?;

  if [INDEX, DOCKER_INDEX].contains(&image.media_type().as_str()) {
    let manifest = document["manifests"]
      .as_array()
      .into_iter()
      .flatten()
      .find(|manifest| {
        manifest["platform"]["architecture"] == platform.architecture.as_str()
          && manifest["platform"]["os"] == platform.os.as_str()
      })
      .and_then(|manifest| manifest["digest"].as_str())
      .ok_or_else(|| {
        EngineError::unavailable(
          format!("read {}", image.reference()),
          format!("no manifest for {}/{}", platform.os, platform.architecture),
        )
      })?
      .to_string();

    document = read(&manifest)?;
  }

  let config = document["config"]["digest"]
    .as_str()
    .ok_or_else(|| EngineError::failed(format!("read {}", image.reference()), "its manifest names no config"))?
    .to_string();

  Ok(match read(&config)?["config"].take() {
    Value::Object(config) => config,
    _ => Map::new(),
  })
}

/// `config`'s `key`, as strings. Empty when it has none.
pub fn strings(config: &ImageConfig, key: &str) -> Vec<String> {
  config
    .get(key)
    .and_then(Value::as_array)
    .into_iter()
    .flatten()
    .filter_map(|value| value.as_str().map(str::to_string))
    .collect()
}

/// `config`'s `key`, as a string.
pub fn string(config: &ImageConfig, key: &str) -> Option<String> {
  config.get(key).and_then(Value::as_str).map(str::to_string)
}

/// What a built image's config is written from.
pub struct Built {
  /// The exported rootfs, as a tar.
  pub layer: PathBuf,
  /// Where the documents describing it are written before they are stored.
  pub scratch: PathBuf,
  pub config: ImageConfig,
  pub platform: Platform,
}

/// Stores `built` as an image of one layer, returning the descriptor of the
/// index a reference is to point at.
pub fn ingest(content: &LocalContentStore, built: Built) -> Result<Descriptor, EngineError> {
  let index = Arc::new(Mutex::new(None));
  let written = Arc::clone(&index);

  content.ingest(move |directory| {
    let writer = ContentWriter::new(directory)?;
    let document = |name: &str, value: Value| -> Result<(i64, String), containerization_framework::Error> {
      let path = built.scratch.join(name);

      std::fs::write(&path, value.to_string())
        .map_err(|error| containerization_framework::Error::failed(format!("write {}", path.display()), error))?;

      let stored = writer.create(&path);
      let _ = std::fs::remove_file(&path);

      stored
    };

    let (size, layer) = writer.create(&built.layer)?;
    let layer = descriptor(LAYER, layer, size);
    let platform = platform(&built.platform);

    let mut config = platform.clone();
    config["config"] = Value::Object(built.config);
    config["rootfs"] = json!({ "type": "layers", "diff_ids": [layer["digest"]] });
    let (size, digest) = document("config.json", config)?;
    let config = descriptor(CONFIG, digest, size);

    let (size, digest) = document(
      "manifest.json",
      json!({ "schemaVersion": 2, "mediaType": MANIFEST, "config": config, "layers": [layer] }),
    )?;
    let mut manifest = descriptor(MANIFEST, digest, size);
    manifest["platform"] = platform;

    let (size, digest) = document(
      "index.json",
      json!({ "schemaVersion": 2, "mediaType": INDEX, "manifests": [manifest] }),
    )?;

    *written.lock().unwrap_or_else(PoisonError::into_inner) = Some(Descriptor::new(INDEX, digest, size));

    Ok(())
  })?;

  let stored = index.lock().unwrap_or_else(PoisonError::into_inner).take();

  stored.ok_or_else(|| EngineError::failed("store the built image", "nothing was written"))
}

fn descriptor(media_type: &str, digest: String, size: i64) -> Value {
  json!({ "mediaType": media_type, "digest": digest, "size": size })
}

fn platform(platform: &Platform) -> Value {
  let mut value = json!({ "architecture": platform.architecture, "os": platform.os });

  if let Some(variant) = &platform.variant {
    value["variant"] = json!(variant);
  }

  value
}

/// Whether every blob under the index `descriptor` names is still stored: a
/// build's blobs go once nothing refers to them.
pub fn holds(content: &LocalContentStore, descriptor: &Descriptor) -> bool {
  let read = |digest: &str| -> Option<Value> {
    let data = content.get(digest).ok()??.data().ok()?;

    serde_json::from_slice(&data).ok()
  };

  let Some(index) = read(&descriptor.digest) else {
    return false;
  };

  index["manifests"]
    .as_array()
    .into_iter()
    .flatten()
    .all(|entry| {
      let Some(manifest) = entry["digest"].as_str().and_then(read) else {
        return false;
      };

      manifest["layers"]
        .as_array()
        .into_iter()
        .flatten()
        .chain([&manifest["config"]])
        .all(|blob| {
          blob["digest"]
            .as_str()
            .is_some_and(|digest| matches!(content.get(digest), Ok(Some(_))))
        })
    })
}

/// A descriptor as the build cache keeps it.
pub fn to_json(stored: &Descriptor) -> String {
  descriptor(&stored.media_type, stored.digest.clone(), stored.size).to_string()
}

/// The descriptor [`to_json`] wrote.
pub fn from_json(text: &str) -> Option<Descriptor> {
  let value: Value = serde_json::from_str(text).ok()?;

  Some(Descriptor::new(
    value["mediaType"].as_str()?,
    value["digest"].as_str()?,
    value["size"].as_i64()?,
  ))
}

/// `ENV` semantics: `additions` override the base's variables of the same
/// name, and everything else the base declares is kept, in the base's order.
pub fn merge(base: Vec<String>, additions: &[String]) -> Vec<String> {
  let name = |variable: &str| variable.split('=').next().unwrap_or_default().to_string();
  let overridden: Vec<String> = additions.iter().map(|variable| name(variable)).collect();

  base
    .into_iter()
    .filter(|variable| !overridden.contains(&name(variable)))
    .chain(additions.iter().cloned())
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn overrides_a_base_variable_and_keeps_the_rest_in_order() {
    let base = vec![
      "PATH=/usr/bin".to_string(),
      "LANG=C".to_string(),
      "HOME=/root".to_string(),
    ];
    let additions = ["LANG=C.UTF-8".to_string(), "EDITOR=vi".to_string()];

    assert_eq!(
      merge(base, &additions),
      ["PATH=/usr/bin", "HOME=/root", "LANG=C.UTF-8", "EDITOR=vi"]
    );
  }

  #[test]
  fn keeps_a_descriptor_as_it_wrote_it() {
    let descriptor = Descriptor::new(INDEX, "sha256:aa", 42);

    assert_eq!(from_json(&to_json(&descriptor)), Some(descriptor));
    assert_eq!(from_json("{\"truncated"), None);
  }

  #[test]
  fn reads_strings_from_a_config_and_nothing_from_what_it_lacks() {
    let config: ImageConfig = serde_json::from_str(r#"{"Env":["PATH=/bin"],"User":"claude"}"#).expect("a config");

    assert_eq!(strings(&config, "Env"), ["PATH=/bin"]);
    assert_eq!(strings(&config, "Cmd"), Vec::<String>::new());
    assert_eq!(string(&config, "User"), Some("claude".to_string()));
    assert_eq!(string(&config, "WorkingDir"), None);
  }
}
