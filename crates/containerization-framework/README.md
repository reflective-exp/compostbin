# containerization-framework

Rust bindings for Apple's [Containerization](https://github.com/apple/containerization)
framework: Linux containers, each in its own lightweight VM, in-process and
with no daemon.

```rust
use containerization_framework::{BootSpec, ExecRequest, Network, Resources, Session, Stdio, Store};

let session = Session::new(Store::at("/Users/me/.cache/containers"));

session.boot(&BootSpec {
    arguments: vec!["/bin/sleep".into(), "infinity".into()],
    ..BootSpec::new(
        "example",
        "docker.io/library/debian:stable-slim",
        Resources { cpus: 4, memory_in_bytes: 4 << 30 },
        Network {
            ipv4_address: "192.168.64.7/24".into(),
            ipv4_gateway: "192.168.64.1".into(),
        },
    )
})?;

let code = session.exec(&ExecRequest::new(
    "example",
    "hello",
    vec!["/bin/echo".into(), "hello".into()],
    Stdio::inherit(false),
))?;
```

A container belongs to the process that booted it and dies with it. Nothing
lists containers, though other processes may join running ones.

## Requirements

- macOS 26 on Apple silicon, and Xcode 26 to build.
- Network on a first build: the build script compiles the bundled Swift package,
  which resolves Containerization and its dependencies through SwiftPM. Versions
  are pinned by the `Package.resolved` that ships with this crate.

On non-macOS platforms, this crate compiles but returns errors on every call.

## Codesigning

**A binary using this crate must carry the `com.apple.security.virtualization`
entitlement.** Without it Virtualization.framework refuses to start a VM, and
`Session::boot` fails saying so.

A `containerization.entitlements` file ships with this crate; binaries compiled
against `containerization-framework` should pass it, or a copy of it, to `codesign`
after compilation.

```sh
cargo build --release
codesign --force --sign - --entitlements containerization.entitlements \
  target/release/your-binary
```

Signing ad hoc (`--sign -`) satisfies the entitlement but gives the binary a new
code identity on every rebuild, so anything keyed to that identity — Keychain
access, for one — prompts again. Sign with a development identity to keep it
stable.

A rebuild drops the signature, so this runs after every build.

## Linking

The Swift runtime this links against is dynamic and referenced as
`@rpath/libswift_Concurrency.dylib`, which dyld resolves against `/usr/lib/swift`
in macOS. Anything that links this crate — a binary of yours, and the
test binaries of any crate of yours that links it — needs that rpath, or it
links and then dies in dyld at launch.

A build script's link arguments reach only its own package's targets, so the
rpath belongs in `.cargo/config.toml`, where a rustflag covers every kind of
target:

```toml
[target.'cfg(target_os = "macos")']
rustflags = ["-C", "link-arg=-Wl,-rpath,/usr/lib/swift"]
```

## Shape

- [`Store`] is an image store on disk, in Containerization's layout.
- [`Builder`] provisions it (a kernel and the `vminitd` init image) and turns a
  [`BuildPlan`] into an image: pull the base, unpack it to a writable ext4
  block, boot it, run each step, and store the result as a single-layer image.
  No daemon, no builder image, no Dockerfile.
- [`Session`] boots a [`BootSpec`]'s container from an image and runs
  [`ExecRequest`]s in it.

Build caching is by rootfs snapshot rather than by layer: a rebuild resumes from
the deepest step whose `cache_key` still matches. This crate only stores and
compares them -- caching, cache invalidation, etc. are the responsibility of
callers.

## Unimplemented

The framework is larger than these bindings. Not exposed: signals to a guest
process, `LinuxPod` (several containers in one VM), container statistics,
filesystem freeze/thaw/trim, host↔guest file copy, registry authentication and
push, OCI layout import/export, and Rosetta (so no linux/amd64 — arm64 only).

## Versioning

The Containerization release and the kernel are pinned separately, and a
mismatch fails at runtime rather than at build time. `Session::version()` names
both.

## License

MIT. Containerization itself is Apache-2.0 and is fetched at build time, not
vendored here.
