---
name: compostbin-manifest
description: Schema for editing .config/compostbin.toml, the compostbin manifest. Use before adding or changing any key in it.
---

# compostbin.toml

`.config/compostbin.toml`, in the project root. Read on the host when the
container is created: edits take effect only after the user exits and runs
`compostbin run` again (`-- --continue` resumes). Every key is optional; an
unknown key is an error at the next start. Paths are host paths; `~` allowed.

With `--profile <name>`, the session reads
`~/.config/compostbin/profiles/<name>.toml` instead (same schema); the briefing
names the file in use.

## Host commands

``` toml
[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]
```

Runs as `compostbin-host test`, on the host, in the project directory. `argv` is
not run through a shell. Exact by default; `arguments`, `deny` and `tty` are in
`references/host.md`. The bare `[host]` keys must precede every
`[host.commands.<name>]`.

## Reference

Read the file for the table being edited:

- `references/host.md`: `[host.commands.<name>]`, `[host]` (clipboard,
  concurrency, ports)
- `references/container.md`: `[project]`, `[container]`, `[image]`
- `references/mounts.md`: `[[paths]]`, `[workspace]`, `compostbin.local.toml`
- `references/claude.md`: `[claude]`
