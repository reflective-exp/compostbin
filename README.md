# Compost bin

Run Claude Code in a container.

compostbin uses Apple's
[Containerization](https://github.com/apple/containerization) framework to
attempt to contain Claude, building the image as well as running it. The current
directory is bind-mounted into the guest, so edits land directly on the host with
no syncing.

Claude can only reach what is mounted. Builds and tests can be configured as
host commands Claude may run, so the toolchain need not be duplicated in the
Debian image.

macOS only.

## Install

``` sh
brew install reflective-exp/tap/compostbin
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

Everything a session can see lands under `/workspace` in the guest.

### init

`compostbin init` writes a default `.config/compostbin.toml`, meant to be checked
in; see [Configuration](#configuration). Uncommitted settings go in
`.config/compostbin.local.toml`; see [Local configuration](#local-configuration).

### build

`compostbin build` builds the base image: Debian, Claude Code, and the tools a
session needs. One image serves every project (unless project-specific overrides
are configured), so it is rarely rebuilt.

The first build provisions the store at `~/.cache/compostbin/images`, downloading
a kernel from
[Kata Containers](https://github.com/kata-containers/kata-containers/releases)
and pulling the `vminit` image from ghcr.io.

If the manifest has an `[image]` table, a project image is built on top of the
base with those additions (language toolchains, private CAs, other tools).

Each build runs every step: there is no build cache.

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

compostbin serves the session's host commands while Claude is attached; both
stop when Claude exits.

`--entrypoint` runs something else in Claude's place, taking the arguments after
`--`; `-U`/`--user` picks the guest user (default `claude`). Handy for working
out what the manifest needs:

``` sh
compostbin run -U root --entrypoint bash
compostbin run -U root --entrypoint apt-cache -- search ripgrep
```

Whichever `run` creates the container owns it: when that process exits, the
container goes, with everything that joined it. A failing `[container] setup`
line stops only Claude. Changes to the container (an `apt-get install`) die with
it; keep them in `[image]`.

### shell

`compostbin shell` opens a bash prompt in a running container, as the
unprivileged `claude` user, in the project's directory under `/workspace`.
`-U`/`--user` opens it as another user the image knows, such as `root`. To
start the container at a shell, use `run --entrypoint bash`.

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
holding credentials. This is a guardrail, not a boundary; the container runs
with your privileges either way.

### ls

`compostbin ls` lists each mounted host path, its guest location, and its source:
the project directory, a workspace root, a `[[paths]]` entry, or a local one.

### clean

`compostbin clean` removes a session's transient state. `--all` also removes the
Claude home (discarding old conversations) and the session's credentials.

## Configuration

`.config/compostbin.toml` declares what the project needs. Every table is
optional.

``` toml
[project]
image = "compostbin/base:latest"
name  = "compostbin"

[container]
cpus   = 4
memory = "8G"
env    = ["GITHUB_TOKEN"]
# Run in the project directory when the container is created, before Claude.
# A failing line stops the session.
setup  = ["direnv allow"]

# Trees whose contents may be mounted. Empty by default, so mounting a whole
# workspace read-write is opt-in. The current directory is always mounted.
[workspace]
roots = ["~/workspace"]

# One more path, this one read-only.
[[paths]]
readonly = true
source   = "~/.cargo/registry"

# This project's image only, on top of the shared base. Order: `packages`
# (as root), `run_as_root` lines, then `run` lines as the `claude` user.
[image]
packages    = ["ca-certificates", "direnv"]
run         = ["""echo 'eval "$(direnv hook bash)"' >> ~/.bashrc"""]
run_as_root = ["update-ca-certificates"]
```

### Local configuration

`.config/compostbin.local.toml` holds uncommitted configuration; add it to
`.gitignore`.

``` toml
[[paths]]
source = "~/code/vendor/libfoo"

[[paths]]
readonly = true
source   = "~/notes"
```

Local configuration currently only supports `[[paths]]`.

### Signing in

Your Claude Code token is read from the login Keychain and seeded into the
session, so the container is logged in as your host is. Otherwise, set
`ANTHROPIC_API_KEY` or log in interactively once; the login persists with the
Claude home.

``` toml
[claude]
seed_from_keychain = true
# Beyond CLAUDE.md, settings.json and skills, which every session gets.
shared             = ["agents"]
```

Your `~/.claude/CLAUDE.md`, `settings.json` and `skills` are copied in on every
run. The host copies are authoritative; edit them there.

### Host commands

The container carries no toolchain; the guest asks the host to run declared
commands instead:

``` toml
[host]
# How many may run at once: subagents call `compostbin-host` independently.
concurrency = 8

[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

# Widened deliberately, for the iterate-on-one-failing-test loop.
[host.commands.test-one]
arguments = true
argv      = ["cargo", "nextest", "run"]
deny      = ["--config", "--manifest-path", "-Z"]
tty       = true
```

Inside the session, `compostbin-host test` runs it on the host as if run
locally: stdin goes in, stdout and stderr come back, and its exit status is
returned. The guest sends only the name, never a command line.

- `arguments` lets the guest append arguments. Off by default, so a command is
  exact unless deliberately widened.
- `deny` refuses guest arguments that would point a widened command at other
  code or configuration. Empty by default, since the relevant flags depend on
  the toolchain. `--config` also refuses `--config=x`; a single-letter `-Z` also
  refuses the joined `-Zx`.
- `tty` runs the command under a pty, so colour and progress work. This merges
  stdout and stderr.
- The spool is only created and mounted when `[host.commands]` is present.

### Host ports

Host services on fixed ports, such as MCP servers, reach the guest at its own
`localhost`:

``` toml
[host]
ports = [7001, 7002]
```

Each port is relayed through a unix socket in the session directory, carried
into this container only: nothing listens on a network address, and **no other
container on this Mac can reach a forwarded port**. Changing ports takes a
restart, since relays are set up when the VM starts.

`run` holds the sockets for the session's lifetime, on a thread beside the
host-command agent. Both end when Claude exits, as does the VM.

A port whose host service hasn't started yet (e.g. one started later via
`compostbin-host`) is normal, so once Claude is attached the relay logs to
`ports.log` in the session directory instead of Claude's terminal. `doctor`
reports ports with nothing behind them.

### Clipboard

Copying inside the session can reach the macOS clipboard:

``` toml
[host]
clipboard = true
```

The image's `pbcopy`, `xclip`, `xsel` and `wl-copy` forward their input to the
host's `pbcopy` over the host-command channel, so Claude's `/copy` (or copying a
response) and `some-command | pbcopy` in `compostbin shell` reach your Mac's
clipboard. The session sets `WAYLAND_DISPLAY`, which is what makes Claude look
for `wl-copy`.

Write-only: the host clipboard never reaches the guest. Off by default; changes
take a restart.

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

`Engine` has no `build`: image building is `compostbin_engine::builder`,
separate from sessions.

### Who owns the VM

A `LinuxContainer` dies with the process that created it, so `compostbin run`
**is** the session, and `shell` is its client. There is no `compostbin stop`:
leaving the session ends the VM, and `compostbin clean` removes what it left
behind.

`run` and `shell` talk over a unix socket in the session's state directory. What
crosses is the client's **terminal**, not its bytes: the request carries the
client's tty descriptor via `SCM_RIGHTS`, and `run` hands it to the guest
process as stdio. The guest talks to the real terminal, nothing relays
keystrokes, and attaching works the same whether the terminal is `run`'s own or
came over the socket. The socket carries only the request, a nudge on each
window resize, and the exit code.

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

The Swift side lives in `swift/CompostbinContainerization`, bridged to Rust by
`crates/containerization-framework-bridge` with
[swift-bridge](https://github.com/chinedufn/swift-bridge). It needs Xcode 26
and macOS 26; `cargo build` runs `swift build` from the bridge crate's
`build.rs`.

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
`crates/containerization-framework-bridge/src/store.rs` and the package version
pinned in `swift/CompostbinContainerization/Package.swift` are two ends of one
protocol (guest agent and library) and must move together.

## Development

``` sh
brew bundle             # medic and its extensions
bin/dev/start           # build, restart the session, attach Claude
medic test              # the whole suite, then a strict check for warnings
medic audit             # audit, check, clippy, format
medic shipit            # all of the above, then a release build, then push
```
