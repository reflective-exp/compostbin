# Host access

The container carries no toolchain. What the session needs from the host
(builds, tests, the clipboard, local services) is declared under `[host]`.
Changes take effect when the container is next created.

## Host commands

``` toml
[host.commands.test]
argv = ["cargo", "nextest", "run", "--workspace"]

# Widened deliberately, for the iterate-on-one-failing-test loop.
[host.commands.test-one]
arguments = true
argv      = ["cargo", "nextest", "run"]
deny      = ["--config", "--manifest-path", "-Z"]
tty       = true
```

Inside the session, `compostbin-host test` runs `argv` on the host, in the
project directory, with the user's environment, as if run locally: stdin goes
in, stdout and stderr come back, and its exit status is returned. The guest
sends only the name, never a command line.

- `argv` (required): the program and its arguments. Not run through a shell, so
  pipes, `&&` and globs are literal; use `["sh", "-c", "…"]` for those.
- `arguments` (`false`): lets the guest append arguments. Off, any argument is
  refused, so a command is exact unless deliberately widened.
- `deny` (`[]`): arguments refused even with `arguments`, for flags that would
  point the command at other code or configuration. `--config` also refuses
  `--config=x`; a single-letter `-Z` also refuses the joined `-Zx`.
- `tty` (`false`): runs under a pty when the guest's stdout is a terminal, so
  colour and progress work. Merges stderr into stdout, so leave it off for
  output a program parses.

Prefer an exact command per task: `arguments` lets the guest choose what runs
on the host.

## [host]

These bare keys must precede every `[host.commands.<name>]` table: TOML assigns
a key to the table most recently opened.

``` toml
[host]
clipboard   = true
concurrency = 8
ports       = [7001, 7002]
```

- `clipboard` (`false`): serves `compostbin-host clipboard` as the host's
  `pbcopy`. See [Clipboard](#clipboard).
- `concurrency` (`8`): how many host commands may run at once. Subagents call
  `compostbin-host` independently.
- `ports` (`[]`): host loopback ports the guest reaches at its own `localhost`.
  See [Ports](#ports).
- `sockets` (`{}`): host Unix sockets by name, as `[host.sockets.<name>]`
  tables. See [Sockets](#sockets).

The host-command channel is only created and mounted when a command or the
clipboard is declared.

## Clipboard

The image's `pbcopy`, `xclip`, `xsel` and `wl-copy` forward their input to the
host's `pbcopy`, so Claude's `/copy` and `some-command | pbcopy` in
`compostbin shell` reach the Mac's clipboard. The session sets
`WAYLAND_DISPLAY`, which is what makes Claude look for `wl-copy`.

Write-only: the host clipboard never reaches the guest. A declared
`[host.commands.clipboard]` replaces the built-in one.

## Ports

Host services on fixed ports, such as MCP servers, answer at the same address
in the guest. Each port is relayed over vsock between this container's VM and
the `compostbin run` that started it: nothing listens on a network address, and
no other container on the Mac can reach a forwarded port.

A port with no service behind it yet (one started later through
`compostbin-host`, say) is normal: once Claude is attached, the relay logs to
`ports.log` in the session directory rather than Claude's terminal.
`compostbin doctor` reports ports with nothing behind them.

## Sockets

``` toml
[host.sockets.notes]
source = "~/Library/Application Support/Notes/mcp.sock"

[host.sockets.agent]
source = "~/.agent.sock"
target = "/run/agent.sock"
mode   = "0600"
```

`[host.sockets.<name>]` relays a host Unix socket into the guest over vsock,
like a port. Names are letters, digits, `-`, `_` and `.`, not starting with
`.`.

- `source` (required): the socket on the host. `~` is allowed.
- `target` (`/run/compostbin/sockets/<name>`): where it answers in the guest,
  an absolute path of letters, digits, `-`, `_`, `.` and `/`, without `..` and
  outside every mount, since the relay replaces whatever is there. Its
  directories are made as needed.
- `mode` (`"0660"`): its permissions in the guest. It belongs to the
  `compostbin` group, which the session's user is in, so `0660` lets Claude
  and root connect, and no other guest user.

With sockets, the guest relay runs as root to make them. Claude and other
attached processes still run as the image's user.

An MCP server served on a socket is a stdio server in the guest, through the
image's `socat`:

``` sh
claude mcp add notes -- socat - UNIX-CONNECT:/run/compostbin/sockets/notes
```

Claude starts and stops `socat` with the session, so nothing outlives it on the
host.

A socket with nothing behind it is normal too: the relay logs to `ports.log`,
and `compostbin doctor` reports it. Sockets are fixed when the container is
created, like ports.
