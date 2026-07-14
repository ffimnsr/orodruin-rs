# orodruin

`orodruin` is a cross-platform CLI for managing project-specific container environments with one consistent workflow.

It reads an `orodruin.toml` file, resolves an environment, and then delegates container operations to the runtime that matches your OS:

- macOS uses Apple Container CLI via `container`
- Linux uses Podman via `podman`

The same `orodruin` commands work on both platforms, but a few backend-only subcommands are only available on macOS. On Linux, unsupported Apple-only commands are hidden from help and return a clear error if invoked directly.

## What You Can Do

- Create a starter config with `orodruin init`
- Build, create, enter, inspect, and remove environment containers
- Run commands inside an environment with `orodruin run`
- Forward common container, image, registry, volume, network, builder, system, and machine commands to the active runtime
- Generate shell completions and print build/version info

## Requirements

- Rust toolchain (`cargo`, `rustc`, `rustup`)
- Linux: Podman installed and available on your `PATH`
- macOS: Apple Container CLI installed and available on your `PATH`

## Install

If you want to install the binary from this repository, use Cargo directly:

```sh
cargo install --path crates/orodruin
```

That installs the CLI binaries into Cargo's binary directory, usually `~/.cargo/bin`.
The primary command is `orodruin`; `rui` is provided as a shorter alias.

## Build From Source

Clone the repository and build the release binary:

```sh
git clone <repository-url>
cd orodruin-rs
cargo build --release -p orodruin
```

The compiled binaries will be available at `target/release/orodruin` and `target/release/rui`.

Run the library tests while you are here:

```sh
cargo test -p orodruin-cli --lib
```

## Quick Start

1. Create a starter configuration:

```sh
orodruin init
```

This creates `orodruin.toml` in the current directory if it does not already exist.

2. Review or customize the config.

Here is a minimal example with one image-based environment and one build-based environment:

```toml
[project]
name = "demo"
default_env = "dev"

[envs.dev]
image = "ubuntu:latest"
project_mount = "/workspace/demo"
workdir = "/workspace/demo"
shell = ["/bin/bash"]
startup_command = ["sleep", "infinity"]
preserve_env = ["SSH_AUTH_SOCK"]

[envs.api]
build = { context = ".", file = "Dockerfile", tag = "demo-api:latest" }
project_mount = "/workspace/demo"
workdir = "/workspace/demo"
shell = ["/bin/bash"]
```

3. Create the environment container:

```sh
orodruin create dev
```

4. Enter the environment:

```sh
orodruin enter dev
```

5. Run a one-off command in the container:

```sh
orodruin run dev -- bash -lc 'pwd && id'
```

## How Configuration Works

- `orodruin` looks for `orodruin.toml` in the current directory and parent directories unless you pass `--config <path>`.
- The config must define at least one environment under `[envs.<name>]`.
- Every environment must define exactly one of `image` or `build`.
- Use `project.default_env` when you want `enter` to work without explicitly naming an environment.

## Runtime Selection

You do not pick the runtime manually.

- On macOS, `orodruin` delegates to Apple Container CLI
- On Linux, `orodruin` delegates to Podman

That means the same user-facing command can fan out to a different backend command depending on your platform.

Examples:

- `orodruin pull alpine:latest` runs `podman pull alpine:latest` on Linux and `container image pull alpine:latest` on macOS
- `orodruin images` runs `podman images` on Linux and `container image list` on macOS
- `orodruin cp src dst` runs `podman cp src dst` on Linux and `container copy src dst` on macOS

## Command Reference

### Project Workflow Commands

| Command | What it does | Example |
| --- | --- | --- |
| `orodruin init` | Create a starter `orodruin.toml` in the current directory | `orodruin init` |
| `orodruin create <env>` | Create the environment container and start it if needed | `orodruin create dev` |
| `orodruin enter [env]` | Open an interactive shell in the container | `orodruin enter` |
| `orodruin run <env> -- <command>` | Run a command inside the environment container | `orodruin run dev -- cargo test` |
| `orodruin list` | Show configured environments and container state | `orodruin list` |
| `orodruin rm <env>` | Remove an environment container | `orodruin rm dev` |
| `orodruin inspect <env>` | Show the resolved config and container details | `orodruin inspect dev` |

Notes:

- `list --json` prints structured JSON for scripting.
- `inspect <env> --json` prints the resolved environment and container inspect payload as JSON.
- Global `--yes` auto-approves prompts such as starting the Apple container system.

Notes:

- `enter` defaults to `project.default_env` or the only configured environment when possible.
- `run` expects the command after `--`.

### Runtime Passthrough Commands

These commands forward directly to the active runtime. The `orodruin` syntax is the same on both platforms, but the backend command differs.

| Command | Linux / Podman | macOS / Apple Container |
| --- | --- | --- |
| `orodruin pull <image>` | `podman pull <image>` | `container image pull <image>` |
| `orodruin images` | `podman images` | `container image list` |
| `orodruin rmi <image>` | `podman rmi <image>` | `container image delete <image>` |
| `orodruin ps` | `podman ps` | `container list` |
| `orodruin logs [args...]` | `podman logs [args...]` | `container logs [args...]` |
| `orodruin build <args...>` | `podman build <args...>` | `container build <args...>` |
| `orodruin cp <src> <dst>` | `podman cp <src> <dst>` | `container copy <src> <dst>` |
| `orodruin login [args...]` | `podman login [args...]` | `container registry login [args...]` |
| `orodruin logout [args...]` | `podman logout [args...]` | `container registry logout [args...]` |

Examples:

```sh
orodruin pull ubuntu:latest
orodruin images
orodruin ps -a
orodruin logs my-container
orodruin build -t demo .
orodruin cp ./local.txt my-container:/tmp/local.txt
orodruin login registry.example.com
orodruin logout registry.example.com
```

### Grouped Subcommands

`orodruin` also exposes container runtime subcommand groups. These are helpful when you want a closer match to the underlying CLI.

| Group | Example |
| --- | --- |
| `orodruin image pull <image>` | `orodruin image pull alpine:latest` |
| `orodruin image list` | `orodruin image list` |
| `orodruin image inspect <image>` | `orodruin image inspect alpine:latest` |
| `orodruin image load [args...]` | `orodruin image load -i image.tar` |
| `orodruin image remove <image>` | `orodruin image remove alpine:latest` |
| `orodruin image push <image>` | `orodruin image push registry.example.com/demo:latest` |
| `orodruin image prune [args...]` | `orodruin image prune --all` |
| `orodruin image save [args...]` | `orodruin image save alpine:latest -o alpine.tar` |
| `orodruin image tag <image>` | `orodruin image tag alpine:latest demo:latest` |
| `orodruin container create [args...]` | `orodruin container create --name demo alpine:latest` |
| `orodruin container exec [args...]` | `orodruin container exec -it demo -- bash` |
| `orodruin container export [args...]` | `orodruin container export demo > demo.tar` |
| `orodruin container kill [args...]` | `orodruin container kill demo` |
| `orodruin container list` | `orodruin container list` |
| `orodruin container inspect <container>` | `orodruin container inspect demo` |
| `orodruin container logs [args...]` | `orodruin container logs demo` |
| `orodruin container prune [args...]` | `orodruin container prune --all` |
| `orodruin container remove <container>` | `orodruin container remove demo` |
| `orodruin container run [args...]` | `orodruin container run --name demo alpine:latest sleep infinity` |
| `orodruin container start <container>` | `orodruin container start demo` |
| `orodruin container stats [args...]` | `orodruin container stats demo` |
| `orodruin container stop <container>` | `orodruin container stop demo` |
| `orodruin registry list` | `orodruin registry list` |
| `orodruin registry login [args...]` | `orodruin registry login registry.example.com` |
| `orodruin registry logout [args...]` | `orodruin registry logout registry.example.com` |
| `orodruin volume list` | `orodruin volume list` |
| `orodruin volume create <name>` | `orodruin volume create cache` |
| `orodruin volume inspect <name>` | `orodruin volume inspect cache` |
| `orodruin volume prune [args...]` | `orodruin volume prune` |
| `orodruin volume remove <name>` | `orodruin volume remove cache` |
| `orodruin network list` | `orodruin network list` |
| `orodruin network create <name>` | `orodruin network create demo-net` |
| `orodruin network inspect <name>` | `orodruin network inspect demo-net` |
| `orodruin network prune [args...]` | `orodruin network prune` |
| `orodruin network remove <name>` | `orodruin network remove demo-net` |
| `orodruin builder status` | `orodruin builder status` |
| `orodruin builder remove [args...]` | `orodruin builder remove` |
| `orodruin system df` | `orodruin system df` |
| `orodruin system logs` | `orodruin system logs` |
| `orodruin system status` | `orodruin system status` |
| `orodruin system version` | `orodruin system version` |
| `orodruin machine list` | `orodruin machine list` |
| `orodruin machine inspect` | `orodruin machine inspect` |
| `orodruin machine logs` | `orodruin machine logs` |
| `orodruin machine create` | `orodruin machine create` |
| `orodruin machine run` | `orodruin machine run` |
| `orodruin machine set` | `orodruin machine set` |
| `orodruin machine stop` | `orodruin machine stop` |

### Platform-Specific Notes

Some backend commands are only available on one platform:

- Linux / Podman does not expose `registry list`, `builder start`, `builder stop`, `system dns`, `system kernel`, `system property`, `system start`, `system stop`, or `machine set-default`
- macOS / Apple Container supports those commands and shows them in help

## Handy Commands

```sh
orodruin --help
orodruin --debug list
orodruin list --json
orodruin inspect dev --json
orodruin --yes enter dev
orodruin completions bash
orodruin version
```

## Using with LLM Coding Agents

If you use `orodruin` with agent instruction files such as `AGENTS.md`, `SKILLS.md`, or other repository-specific guidance, tell the agent to run project commands inside the environment container instead of on the host.

A good default instruction looks like this:

```md
## Container Execution

This repository uses `orodruin` for its development environment.

- Run project commands inside the container, not on the host.
- Prefer `orodruin run <env> -- <command>` for one-off commands.
- Prefer `orodruin enter <env>` only when an interactive shell is explicitly needed.
- If `project.default_env` is configured, you may omit `<env>` when appropriate, but using the explicit environment name is preferred in automation.
- Before running project-specific commands, make sure the environment exists by running `orodruin create <env>` if needed.

Examples:

- `orodruin run dev -- cargo test`
- `orodruin run dev -- cargo fmt --check`
- `orodruin run dev -- python -m pytest`
- `orodruin run dev -- bash -lc 'pwd && ls -la'`
```

For most agents, the simplest rule is:

- do not run `cargo`, `python`, `npm`, `go`, or other project tools directly on the host when they are intended to run inside the container
- instead, wrap them with `orodruin run <env> -- ...`

If your repository has a default environment, you can also use shorter forms such as:

```sh
orodruin run -- cargo test
orodruin run -- cargo clippy
```

If you want the agent to be extra safe, add a note like this too:

```md
If a command fails because the environment container does not exist yet, run `orodruin create dev` first and retry the command inside the container.
```

## Development

Useful commands while iterating locally:

```sh
cargo fmt
cargo test -p orodruin-cli --lib
cargo run -p orodruin-cli -- --help
```

## License

This project is distributed under the terms of the repository's license files.
