# The container and its image

Changes take effect when the container is next created; `[image]` changes also
need `compostbin build`.

## [project]

``` toml
[project]
image = "compostbin/base:latest"
name  = "my-project"
```

- `image` (`"compostbin/base:latest"`): the base image.
- `name` (the project directory's name): names the container and its state.

## [container]

``` toml
[container]
cpus   = 4
env    = ["GITHUB_TOKEN"]
memory = "8G"
setup  = ["direnv allow"]
```

- `cpus` (`4`).
- `env` (`[]`): names of host environment variables passed through. Names, not
  values.
- `memory` (`"8G"`): a number with an optional `G`, `M` or `K` suffix, or a
  bare byte count.
- `setup` (`[]`): shell lines run as `claude` in the project directory when the
  container is created, before Claude. A failing line stops the session.

## [image]

This project's additions to the shared base. Any entry means a derived image,
built by `compostbin build`.

``` toml
[image]
packages    = ["ca-certificates", "direnv"]
run_as_root = ["update-ca-certificates"]
run         = ["""echo 'eval "$(direnv hook bash)"' >> ~/.bashrc"""]
```

Run in this order:

- `packages` (`[]`): Debian packages, `apt-get install`ed as root.
- `run_as_root` (`[]`): shell lines run as root, for what only root can do.
- `run` (`[]`): shell lines run as the `claude` user. Anything touching `~`
  belongs here.
