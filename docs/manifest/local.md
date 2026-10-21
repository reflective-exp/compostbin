# Local overrides

`.config/compostbin.local.toml` holds settings one developer wants that the
project should not commit; add it to `.gitignore`. It accepts only
`[container]` and `[[paths]]`. A profile replaces it along with the manifest.

``` toml
[container]
cpus = 8

[[paths]]
source = "~/code/vendor/libfoo"
```

- `[container]`: each key set replaces the manifest's value, lists included;
  keys left out keep the manifest's. See [the container](container.md).
- `[[paths]]`: added to the manifest's. `compostbin add --local` writes here.
  See [Mounts](mounts.md).
