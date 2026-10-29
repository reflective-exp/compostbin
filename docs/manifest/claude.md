# Claude

``` toml
[claude]
shared = ["commands"]
```

- `shared` (`[]`): `~/.claude` entries copied from the host beyond `CLAUDE.md`,
  `agents`, `settings.json` and `skills`, which every session gets on every
  run. Each names a file or directory directly under `~/.claude`; links are
  followed, so the session gets their contents. The host copies are
  authoritative; edit them there.
- `home` (one per project): the host directory holding the session's
  `~/.claude`. Shared between projects, `--continue` resumes whichever ran last.

When the host sets `CLAUDE_CONFIG_DIR`, that directory stands in for
`~/.claude` here and for `compostbin install`, and the token comes from the
Keychain item Claude Code keeps for it.

## Global configuration

`~/.config/compostbin/config.toml` applies to every session, whichever manifest
or profile configures it. It takes only `[claude]`:

``` toml
[claude]
keychain = true
shared   = ["commands"]
```

- `keychain` (`false`): signs the session in with the Claude Code token from
  the host's login Keychain, and saves the session's token back whenever it
  outlasts the Keychain's, so a login or refresh in the container reaches the
  host. Otherwise, set `ANTHROPIC_API_KEY` or log in interactively once; the
  login persists across sessions with the container's Claude home.
- `shared` (`[]`): merges into the project's `[claude] shared`.

Anything `settings.json` refers to by path must be reachable in the guest: the
session's Claude home is `~/.claude` there too, but an absolute host path such
as `/Users/you/...` is not, and a script calling a host-only tool fails.
