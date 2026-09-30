# Compost bin

Run Claude Code in a container.

compostbin uses Apple's
[Containerization](https://github.com/apple/containerization) framework to
run Claude in a container, building the image as well as running it. The current
directory is bind-mounted into the guest, so edits land directly on the host with
no syncing.

Claude can only reach what is mounted. Builds and tests can be configured as
host commands Claude may run, so the toolchain need not be duplicated in the
Debian image.

macOS only.

## Install

``` sh
brew install reflective-exp/tap/compostbin
compostbin install
compostbin init
compostbin doctor
```

## Usage

Every command runs from a project root and reads its manifest at
`.config/compostbin.toml`. A first session:

``` sh
compostbin init         # write the manifest
compostbin build        # build the base image, shared by every project
compostbin doctor       # check the store, the image, credentials, the mounts
compostbin run          # start the session and attach Claude
```

Configured paths mount under `/workspace` in the guest.

### install

`compostbin install` puts compostbin's Claude skills in `~/.claude/skills`.

- `compostbin-manifest` teaches Claude the manifest schema. Also installs
  [`docs/manifest`](docs/manifest) into a reference directory.

It also creates an empty `~/.config/compostbin/profiles` for
[profiles](#profiles).

### init

`compostbin init` writes a default `.config/compostbin.toml`, meant to be checked
in; see [Configuration](#configuration). Uncommitted settings go in
`.config/compostbin.local.toml`; see [Mounts](docs/manifest/mounts.md#local-configuration).

### build

`compostbin build` builds the base image: Debian, Claude Code, and the tools a
session needs.

The first build provisions the store at `~/.cache/compostbin/images`, downloading
a kernel from
[Kata Containers](https://github.com/kata-containers/kata-containers/releases)
and pulling the `vminit` image from ghcr.io.

If the manifest has an `[image]` table, a project image is built on top of the
base with those additions (language toolchains, private CAs, other tools). Without
`[image]` configuration, sessions start from the same base image.

### doctor

`compostbin doctor` checks whether a session would work:

- what runs containers, and whether its image store can be read
- whether the base image has been built
- whether the session can authenticate
- every declared path: that it exists, and whether symlinks escape the mounted
  trees
- every mount, declared or not, for a known dangerous pattern — a hand-edited
  manifest bypasses `add`'s refusal
- whether a running container was started with the mounts the manifest now
  declares
- the host commands the guest is allowed to run

### run

`compostbin run` starts the container and attaches Claude. Arguments after `--`
pass through to `claude`:

``` sh
compostbin run -- --continue --model opus
```

The session's Claude home is per-project, outlives the container, and is the
session's `CLAUDE_CONFIG_DIR`, so `--continue` resumes the last run's
conversation.

Claude gets a terminal whenever the host side has one on both stdin and stdout.
Redirect or pipe either, and the session runs on plain streams instead, so
`echo "..." | compostbin run -- -p` and `compostbin run -- -p > answer.txt`
work as written.

Whichever command creates the container owns it: when that process exits, the
container goes, with everything that joined it. A failing `[container] setup`
line stops only Claude. Changes to the container (an `apt-get install`) die with
it; keep them in `[image]`.

### exec

`compostbin exec <command>` runs something other than Claude in the session,
starting the container if it isn't up. `-U`/`--user` picks the guest user
(default `claude`). Everything after the command belongs to the command:

``` sh
compostbin exec ls -l
compostbin exec -U root apt-cache search ripgrep
```

Its stdin, stdout and stderr are this side's own, so redirections and pipes
behave as the shell wrote them. `-t` asks for a terminal instead, which a
command drawing a UI needs; this side must have one on stdin and stdout, or
`exec` says so and stops. A `[container] setup` line that fails doesn't stop an
`exec`, which may well be what's debugging it.

### shell

`compostbin shell` is `exec -t bash`: a bash prompt in the session, as the
unprivileged `claude` user, in the project's directory under `/workspace`.
`-U`/`--user` opens it as another user the image knows, such as `root`. It
starts the container if nothing else has.

### add

`compostbin add <path>` records a host path as a `[[paths]]` entry, so the next
container mounts it.

``` sh
compostbin add ../libfoo --readonly
```

A path inside a `[workspace] roots` tree is already mounted: `add` says so,
records nothing, and ignores `--readonly`. Any other path needs the container
recreated: exit the session, then `compostbin run -- --continue` resumes the
conversation with the new mount.

`--local` records the entry in `.config/compostbin.local.toml`.

Without `--force`, `add` refuses obvious mistakes: `~`, `/`, `~/.ssh`, anything
holding credentials.

### ls

`compostbin ls` lists each mounted host path, its guest location, and its source:
the project directory, a workspace root, a `[[paths]]` entry, or a local one.

### clean

`compostbin clean` removes a session's transient state. `--all` also removes the
Claude home (discarding old conversations) and the session's credentials.

## Configuration

`.config/compostbin.toml` declares what the project needs. Every table is
optional, and changes take effect when the container is next created.

``` toml
[container]
setup = ["direnv allow"]

[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

# Widened deliberately, for the iterate-on-one-failing-test loop.
[host.commands.test-one]
arguments = true
argv      = ["cargo", "nextest", "run"]
deny      = ["--config", "--manifest-path", "-Z"]
tty       = true

[[paths]]
readonly = true
source   = "~/.cargo/registry"
```

Inside the session, `compostbin-host test` runs `cargo nextest run --workspace`
on the host. Every key is described in `docs/manifest`:

- [Host access](docs/manifest/host.md): host commands, the clipboard, ports
- [The container and its image](docs/manifest/container.md): `[project]`,
  `[container]`, `[image]`
- [Mounts](docs/manifest/mounts.md): `[[paths]]`, `[workspace]`, and the
  uncommitted `.config/compostbin.local.toml`
- [Claude](docs/manifest/claude.md): signing in, and what is copied from
  `~/.claude`

### Profiles

A profile is a manifest kept outside any project, at
`~/.config/compostbin/profiles/<name>.toml`. `--profile <name>` on `run`,
`exec`, `shell`, `build`, `doctor`, `ls` and `clean` reads it instead of the
project's manifest:

``` sh
compostbin run --profile rust -- --continue
```

It replaces the manifest and its local overlay; nothing is merged.
`[project] name` is ignored: the session is named after the directory plus a
hash of its path, so each directory keeps its own container and conversation.
A profile's `[image]` builds one shared image, `compostbin/profile-<name>`.
Profiles are written by hand; `add` and `init` ignore them.

### What the session is told

Inside, a container looks like an ordinary Debian box, with no hint that the
toolchain is missing on purpose or that `compostbin-host` exists. So each
session gets a briefing: where it is, and every command it may ask the host
for, rendered from `[host.commands]` so the two cannot drift.

The briefing is mounted read-only at `/etc/claude-code` (Claude's managed
settings), and a `SessionStart` hook prints it at the start of every session.
Managed settings outrank `~/.claude/settings.json`, which compostbin copies
from the host and never writes to.

The briefing is written when the container is created, so an edited
`[host.commands]` takes effect on restart.

## Containerization.framework

A session is a virtual machine owned by the compostbin process, through
[Containerization](https://github.com/apple/containerization). Builds use the
same library; see [Building images](#building-images).

### Who owns the VM

A `LinuxContainer` dies with the process that created it, so `compostbin run`
**is** the session, and `shell` is its client. There is no `compostbin stop`:
leaving the session ends the VM, and `compostbin clean` removes what it left
behind.

`run` and `shell` talk over a unix socket in the session's state directory. Client
streams cross it — its terminal, or its stdin, stdout and stderr —
and the guest process gets them directly.

When `run` exits, the VM ends, and so does any attached `shell`.

### Building images

A build executes a `BuildPlan` (a base, named steps, and what the finished image
runs as):

1. pull the base into the store, unpack it to a writable `rootfs.ext4`;
2. boot that block with a keepalive process, the same NAT a session gets (`apt`
   needs the network) and the build context mounted read-only;
3. run each step as an `exec` of `bash -euo pipefail -c`, as root or as a named
   user, streaming its output to stderr;
4. stop the VM and export the block back to a tar with `EXT4Reader.export`;
5. ingest that tar as a single-layer image — layer, config, manifest, index — and
   register it under the plan's tag.

The layer is an **uncompressed** tar (`imageLayer`, not `imageLayerGzip`), so the
layer digest equals the diffID. Nothing pushes these images, so compression
gains nothing.

`base_plan` defines the base image; `project_plan` composes a project's
additions from manifest fields.

The export reads the inodes and writes pax with `schily` xattrs, carrying uids,
modes, symlinks, hardlinks and extended attributes into the layer.

### Host ports are relayed, not mounted

`[host] ports` puts one unix socket per port in the session directory and gives
the guest the other end. Containerization configures this as a
`UnixSocketConfiguration` (with its own direction and mode), not a filesystem.

So `RunSpec` keeps them apart: `mounts` for filesystems, `sockets` for relays,
which go in `config.sockets` so a socket can't be mounted as a filesystem by
accident.

### Attached processes are seeded from the image

`ContainerManager.create` builds the first process from the image config, so
the keepalive runs as `claude`. `LinuxContainer.exec` starts from a bare
configuration: uid 0 with only a default `PATH`. Since compostbin attaches
Claude with `exec`, that default would run the session as root with
`HOME=/root` (the runtime resolves `HOME` from the process user's passwd entry)
in an image whose config names `claude`.

So the image config is kept from boot, and every attach seeds itself from it
before applying the session's arguments and environment.

The Swift side lives in `crates/containerization-framework/swift`, bridged to
the crate around it with
[swift-bridge](https://github.com/chinedufn/swift-bridge). It needs Xcode 26
and macOS 26; `cargo build` stages the package into `<target>/<profile>` and runs
`swift build` there from that crate's `build.rs`.

`containerization-framework` is a general binding, published on its own and
holding nothing of compostbin's: it takes a container to boot and an image to
build, and everything it would otherwise have to decide — where a guest sits on
the NAT network, what a builder is called, what shell runs a build step, what
invalidates a cached step — arrives from the caller.
`compostbin-engine::containerization` is where compostbin decides those.

Two non-obvious requirements:

- **The binary must be signed.** Virtualization.framework refuses every call
  without the `com.apple.security.virtualization` entitlement, and a rebuild
  drops the signature, so `bin/dev/sign` runs after every build;
  `bin/dev/start` does both.

  It signs with `DEVELOPMENT_TEAM`, which `.envrc` requires and `.local/envrc`
  supplies. That is a team id (the certificate's OU), which never appears in
  the common name `codesign --sign` matches, so `bin/dev/identity` resolves it
  by reading the certificate. Without it the binary is signed ad hoc: enough
  for the entitlement, but each rebuild gets a new code identity, so the
  keychain holding Claude's credentials re-prompts for access.

- **The store is disposable.** `~/.cache/compostbin/images` uses
  Containerization's `ImageStore` layout: an image index in `state.json`, blobs
  under `content/`, the kernel under `kernels/`, one unpacked rootfs per
  container under `containers/`. `ContainerManager` opens it as-is.
  `compostbin build` provisions whatever is missing, so nothing in it must be
  kept and `clean` never touches it.

The `vminit` reference pinned in
`crates/containerization-framework/src/store.rs` and the package version pinned
in that crate's `swift/Package.swift` are two ends of one protocol (guest agent
and library) and must move together.

## Development

``` sh
brew bundle             # medic and its extensions
bin/dev/start           # build, restart the session, attach Claude
medic test              # the whole suite, then a strict check for warnings
medic audit             # audit, check, clippy, format
medic shipit            # all of the above, then a release build, then push
```

### Integration tests

`crates/compostbin-test` starts real sessions: each test writes a manifest,
runs `compostbin exec` against a project of its own, and asserts on what the
guest saw. They are behind a feature, since they need a built base image and
several minutes of VMs:

``` sh
compostbin build
cargo nextest run --features compostbin-test/integration
```

Each test gets a temporary `HOME`, so its session state, Claude home and shared
settings are its own; only the image store is borrowed from
`~/.cache/compostbin`, which is what makes them quick. The binary under test is
copied to `target/<profile>/compostbin-signed` and signed there, because a test
run rebuilds `compostbin` and a rebuild drops the entitlement.
