# Compost bin

Run Claude Code in a container.

This project uses Apple's [container CLI](https://github.com/apple/container)
to attempt to contain Claude. The current directory is bind-mounted into the
runtime, so that edits land directly in the host with no syncing or copying.

Claude can only reach what is mounted; builds and tests may be configured to
allow Claude Code to execute specific commands on the host--with no need to
duplicate the development toolchain in debian.

This project currently only runs on macOS.

## Install

``` sh
brew bundle
cargo install --path crates/compostbin-cli
container system start
```

## Usage

Every command runs from the root of a project, and reads that project's manifest
at `.config/compostbin.toml`. A first session is four commands:

``` sh
compostbin init         # write the manifest
compostbin build        # build the base image, shared by every project
compostbin doctor       # check the daemon, the image, credentials, the mounts
compostbin run          # start the session and attach Claude
```

Everything a session can see lands under `/workspace` in the guest.

### init

`compostbin init` writes a default `.config/compostbin.toml`. It is checked into
the project it configures; see [Configuration](#configuration).

### build

`compostbin build` builds the base image: debian, node, Claude Code, and the
handful of tools a session needs. One image serves every project, so it is built
once and rebuilt only when compostbin's own Dockerfile changes.

When the manifest has an `[image]` table, a second image is built `FROM` the
base with additions. Extra language toolchains, private CAs, or other project-specific
tools may be configured on top of the base image.

### doctor

`compostbin doctor` checks whether a session would work. It checks:

- the `container` CLI and whether its daemon is responding
- whether container images  have been built
- whether the session can authenticate
- every declared path: that it exists, that it does not declare a known dangerous
  pattern, and whether symlinks escape the mounted trees
- the host commands the guest is allowed to run

### run

`compostbin run` starts the container and attaches to Claude inside it. Anything after
`--` is passed straight through to `claude`:

``` sh
compostbin run -- --continue --model opus
```

The session's Claude home outlives the container, so `--continue` resumes the
conversation from the last run. CLAUDE_HOME is per-project.

While Claude is attached, compostbin serves that session's host commands; both
stop when Claude exits.

### shell

`compostbin shell` opens a bash prompt in a running container, as an unprivileged
user, in `/workspace`.

### add

`compostbin add <path>` mounts another host path into the session and records it
in the manifest.

``` sh
compostbin add ../libfoo --readonly
compostbin add ../libfoo --restart     # recreate the container so it appears immediately
```

A path already inside a `[workspace] roots` tree is mounted as soon as it is
added; anything else needs the container recreated, which `--restart` does.

`add` refuses a path that is obviously a mistake — `~`, `/`, `~/.ssh`, anything
holding credentials — unless you pass `--force`. It is a guardrail against a
slip rather than a boundary: the container runs with your own privileges either
way.

### ls

`compostbin ls` lists what the session mounts: each host path, where it appears
in the guest, and why it is there — the project directory, a workspace root, or
an explicit `[[paths]]` entry.

### stop

`compostbin stop` stops and deletes the session's container. State that outlives
it—the Claude home—is left intact.

### clean

`compostbin clean` removes a session's transient state. With `--all` it also
removes the Claude home, discarding old conversations, and the session's
credentials.

### host-agent

`compostbin host-agent` serves host commands for a session started elsewhere —
`compostbin shell`, or a container you attached to by hand. `run` does this
itself, so this is only for the sessions it did not start.

## Configuration

`.config/compostbin.toml` declares what the project needs. Every table is
optional; a manifest that says nothing gets the defaults.

``` toml
[project]
image = "compostbin/base:latest"
name  = "compostbin"

[container]
cpus   = 4
memory = "8G"
env    = ["GITHUB_TOKEN"]

# Trees whose contents may be mounted. Empty by default: mounting a whole
# workspace read-write is opt-in. The directory you run compostbin in is
# mounted regardless.
[workspace]
roots = ["~/workspace"]

# One more path, this one read-only.
[[paths]]
readonly = true
source   = "~/.cargo/registry"

# Added to this project's image only, on top of the shared base.
[image]
packages = ["direnv"]
run      = ["""echo 'eval "$(direnv hook bash)"' >> ~/.bashrc"""]
```

### Signing in

Your Claude Code token is read from the login Keychain and seeded into the
session, so a container is logged in the way your host is. Failing that, set
`ANTHROPIC_API_KEY` or log in interactively once — the login persists with the
Claude home.

``` toml
[claude]
seed_from_keychain = true
# Beyond CLAUDE.md, settings.json and skills, which every session gets.
shared             = ["agents"]
```

Your own `~/.claude/CLAUDE.md`, `settings.json` and `skills` are copied in on
every run. The host stays authoritative: edit them there, and the next session
has them.

### Host commands

The container carries no toolchain. Instead the guest may ask the host to run
specific declared commands:

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
tty       = true
```

Inside the session, `compostbin-host test` runs it on the host and behaves as
though the guest shell had run it: stdin goes in, stdout and stderr come back,
its exit status is yours. The guest sends the name, never a command line.

- `arguments` lets the guest append its own arguments; off by default, so a
  command is exact unless deliberately widened.
- `tty` runs the command under a pty, so colour and progress work. A terminal is
  one device, so this merges stdout and stderr.
- A project with no `[host.commands]` has no guest-to-host path at all: the
  spool is neither created nor mounted.

### What the session is told

From the inside a container looks like an ordinary Debian box: nothing in it
says the toolchain is missing on purpose, or that `compostbin-host` is the way
out. So each session is briefed on itself — where it is, and every command it
may ask the host for, rendered from `[host.commands]` rather than written by
hand, so the two cannot drift apart.

The briefing is mounted read-only at `/etc/claude-code`, Claude's managed
settings, and a `SessionStart` hook prints it at the top of every session.
Managed settings outrank `~/.claude/settings.json`, which stays yours:
compostbin copies it from the host and never writes to it.

The briefing is written when the container is created, so an edited
`[host.commands]` reaches the session by the restart that serves it.

## Development

``` sh
bin/dev/start           # build, restart the session, attach Claude
medic test              # the whole suite
medic audit             # audit, check, clippy, format
medic shipit            # all of the above, then push
```
