# Local overrides

`.config/compostbin.local.toml` holds settings one developer wants that the
project should not commit; add it to `.gitignore`. It accepts any manifest
key. A profile replaces it along with the manifest.

``` toml
[container]
cpus = 8

[host.commands.test]
argv = ["just", "test"]

[[paths]]
source = "~/code/vendor/libfoo"
```

- Each key set replaces the value from the parent manifest, lists included.
  Tables merge key by key, so `cpus` above leaves the manifest's `memory` alone.
- `[host.commands.<name>]`: replaces the parent manifest's command of that name,
  without merging sub-keys.
- `[host.sockets.<name>]`: merges key by key like any table, so it can set
  just this machine's `source`, keeping the manifest's `target` and `mode`.
- `[[paths]]`: added to the manifest's. `compostbin add --local` writes here.
  See [Mounts](mounts.md).
