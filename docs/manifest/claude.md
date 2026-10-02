# Claude

``` toml
[claude]
seed_from_keychain = true
shared             = ["commands"]
```

- `seed_from_keychain` (`true`): signs the session in with the Claude Code token
  from the host's login Keychain. Otherwise, set `ANTHROPIC_API_KEY` or log in
  interactively once; the login persists with the Claude home.
- `shared` (`[]`): `~/.claude` entries copied from the host beyond `CLAUDE.md`,
  `agents`, `settings.json` and `skills`, which every session gets on every
  run. Each names a file or directory directly under `~/.claude`; links are
  followed, so the session gets their contents. The host copies are
  authoritative; edit them there.
- `home` (one per project): the host directory holding the session's
  `~/.claude`. Shared between projects, `--continue` resumes whichever ran last.

## Global configuration

`~/.config/compostbin/config.toml` applies to every session, whichever manifest
or profile configures it. Its only key, `[claude] shared`, merges into the
project's manifest:

``` toml
[claude]
shared = ["commands"]
```

Anything `settings.json` refers to by path must be reachable in the guest: the
session's Claude home is `~/.claude` there too, but an absolute host path such
as `/Users/you/...` is not, and a script calling a host-only tool fails.
