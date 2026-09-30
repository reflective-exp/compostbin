# Claude

``` toml
[claude]
seed_from_keychain = true
shared             = ["agents"]
```

- `seed_from_keychain` (`true`): signs the session in with the Claude Code token
  from the host's login Keychain. Otherwise, set `ANTHROPIC_API_KEY` or log in
  interactively once; the login persists with the Claude home.
- `shared` (`[]`): `~/.claude` entries copied from the host beyond `CLAUDE.md`,
  `settings.json` and `skills`, which every session gets on every run. The host
  copies are authoritative; edit them there.
- `home` (one per project): the host directory holding the session's
  `~/.claude`. Shared between projects, `--continue` resumes whichever ran last.
