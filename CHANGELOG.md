# Changelog

## Unreleased

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
