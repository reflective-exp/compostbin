# Changelog

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
