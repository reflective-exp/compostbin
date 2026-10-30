# Changelog

## Unreleased

- Add `[host.sockets.<name>]`, relaying a host Unix socket into the guest at
  its `target` (default `/run/compostbin/sockets/<name>`) with its `mode`,
  owned by a new `compostbin` group the session's user is in. An MCP server on
  a socket runs as a stdio server through `socat`. The guest half and the
  group are in the image, so run `compostbin build` before using sockets.
- `doctor` reports sockets with nothing listening behind them.
- A new session waits up to 5s for the guest relay's sockets before running
  anything.

## v0.19.0

- Add `[claude] keychain` to `~/.config/compostbin/config.toml`, defaulted to
  `false`. With `keychain`, save the session's Claude token back to the Keychain
  when a login or refresh in the container leaves it newer than the Keychain's.
  - Copy only the Claude login between the Keychain and the session, leaving
    each side's MCP tokens.
- `doctor` fails credentials that hold no Claude login.
- `doctor` fails a manifest setting a retired key, instead of erroring before
  any check. `doctor --fix` moves `[claude] keychain` and `seed_from_keychain`
  to `~/.config/compostbin/config.toml`.
- `init` writes only `[project] image` and `name`.
- `.config/compostbin.local.toml` takes every manifest key, not only
  `[container]` and `[[paths]]`.
- `add` changes only the file it records in, keeping the other keys that file
  sets.

### Breaking

- Remove `[claude] seed_from_keychain`.

## v0.18.0

- Honor the host's `CLAUDE_CONFIG_DIR`: share settings from it, install skills
  into it, and seed credentials from the Keychain item Claude Code names after
  it.
- Drain the `SessionStart` hook's stdin before printing the briefing.
- Update deps:
  - Closing a terminal attached to an existing container no longer aborts the
    parent session.

## v0.17.0

- Update containerization-framework to v0.10.0.
- Boot the Containerization 0.53.0 init image.
- Also read `[container]` from `.config/compostbin.local.toml`. Each key it
  sets replaces the shared manifest's.

## v0.16.0

- Update containerization-framework to v0.9.0.
- Boot the Containerization 0.51.0 init image.

## v0.15.0

- Update containerization-framework to v0.6.0.
- Boot the Containerization 0.49.0 init image.

## v0.14.0

- Update containerization-framework to v0.4.0.
- Show progress on a terminal while `build` pulls its base or the init image.

### Breaking

- Relay `[host] ports` over vsock instead of a unix socket per port in the
  session directory. The guest half is in the image, so run `compostbin build`
  again before using ports.

## v0.13.0

- Update containerization-framework to v0.3.0.
- Pull in image/cache management that used to live in containerization.

## v0.12.0

- Update containerization framework to v0.2.2.
- Copy `~/.claude/agents` into every session, alongside `CLAUDE.md`,
  `settings.json` and `skills`.
- Read `~/.config/compostbin/config.toml`, which can merge `[claude] shared`
  entries to every session.

## v0.11.0

- Add `--profile <name>` to session commands, reading
  `~/.config/compostbin/profiles/<name>.toml` instead of the project's
  manifest. `compostbin install` creates that directory.
- Version the kernel and initfs in the compostbin cache; automatically
  detect updates.

## v0.10.0

- Add `compostbin install`, which installs or updates compostbin's Claude skills
  in `~/.claude/skills`.
  - `compostbin-manifest`
- Move manifest documentation from the README to `docs/manifest`.

## v0.9.1

- Extract `containerization-framework` to external repo; compostbin uses the
  published crate.
- Create the session's Claude home before the container mounts it. A host with
  no `~/.claude` and `seed_from_keychain = false` had nothing to create it, and
  the session failed to boot on a mount source that did not exist.

## v0.9.0

- Add `compostbin exec <command>`, which runs something other than Claude in the
  session, with `-t` for a terminal and `-U` / `--user <user>` for the guest
  user. `run` always attaches Claude, and `shell` is now `exec -t bash`.
- Attach every process to the caller's own stdin, stdout and stderr, instead of
  running it with no streams at all when the caller has no terminal. Piping a
  prompt in, or redirecting output to a file, now works, whether the command
  creates the container or joins one already running.
- Run `[container] setup` lines without the caller's stdin, which they could
  otherwise read before the attached process started.
- Host terminal resizes get passed through to guest processes.

## v0.8.1

- Specify a small, random container ID for the container builder.

## v0.8.0

- **Breaking:** build images with Containerization instead of `container build`
  and BuildKit. Removes last references to Apple's `container` CLI.
  - Requires rebuilding images!
- **Breaking:** remove `compostbin stop`, `compostbin port-relay` and
  `compostbin host-agent` -- these commands were all around controlling
  Apple's `container` CLI.
- Unpack each image once and clone it for every `compostbin run`, instead of
  unpacking it again on each run. Starting a session is near-instant after the
  first, and sessions on one image share disk blocks.
- Add `[container] setup`, shell lines run as `claude` in the project directory
  each time the container is created, before Claude starts.
- Add `-U` / `--user <user>` to `shell`. Defaults to the claude user.
- Fix `[container] env` not reaching Claude or `setup`.

## v0.7.0

- Also read `[[paths]]` from `.config/compostbin.local.toml`, which may be
  ignored from git. Adds `--local` option to `compostbin add`.

## v0.6.0

- Relay `[host] ports` through a unix socket per port instead of the vmnet
  gateway, so a forwarded port reaches the session's container and nothing else.
  Needs a `compostbin build`.
- Hold the port sockets in a relay that lives as long as the container rather
  than as long as the attached session, so a second `compostbin run` keeps its
  ports.
- Add a `host ports` check to `doctor`, which names a port nothing is listening
  on and a relay that has died.
- Fix a host channel that went silently dead: `clean` and the cleanup after
  Claude exits unlinked the spool directory the running container was mounting,
  so every later `shell` and `host-agent` for that container wrote to a
  directory the guest could no longer reach. Both now empty it in place.
- Install native claude into `/usr/local/bin/claude` and remove node from the
  base container.
- Remove build-essential from base docker file.

## v0.5.0

- Add `[host] clipboard` to send the guest's `pbcopy`, `xclip`, `xsel` and
  `wl-copy` to the macOS clipboard. Needs a `compostbin build`.

## v0.4.0

- Harden against container escapes.
- Add `deny` to `[host.commands.*]`, replacing the built-in cargo deny list
  (`--config`, `--manifest-path`, `-Z`). Nothing is denied by default.
- Avoid redundant symlink walks for nested workspace entries.
- Run `tty = true` host commands under a pty only when the caller's stdout is a
  terminal, so captured output has no escape codes.

## v0.3.0

- Add `[host] ports` to relay host ports into containers.

## v0.2.0

- Add `[image] run_as_root` for arbitrary privileged `RUN`
  commands added to the project docker image.

## v0.1.0

- Initial release.
