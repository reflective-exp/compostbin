# Mounts

The project directory is always mounted. Everything mounts under `/workspace`
in the guest, so no host path is visible inside it. Mounts are fixed when the
container is created: exit the session, then `compostbin run -- --continue`
resumes the conversation with the new mount.

## [[paths]]

One more host path, at `/workspace/<basename>`. `compostbin add <path>` records
one.

``` toml
[[paths]]
readonly = true
source   = "~/.cargo/registry"
```

- `source` (required): the host path.
- `readonly` (`false`).
- `target` (`/workspace/<basename>`): an absolute guest path, for the rare case
  where something must appear at a fixed location.

## [workspace]

``` toml
[workspace]
roots = ["~/workspace"]
```

- `roots` (`[]`): host trees mounted whole, read-write, each at
  `/workspace/<basename>`. Empty by default, so mounting a whole workspace is
  opt-in. A path inside a root is already mounted; `add` records nothing for it.

## Local configuration

`.config/compostbin.local.toml` holds mounts one developer wants that the
project should not commit; add it to `.gitignore`. It accepts only
`[[paths]]`. `compostbin add --local` writes there.

``` toml
[[paths]]
source = "~/code/vendor/libfoo"
```
