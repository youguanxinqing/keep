English | [简体中文](README.zh-CN.md)

# keep

`keep` is a lightweight process supervisor for local development. It starts
multiple processes in dependency order, waits for services to become ready,
and lets you inspect or stop them from any directory.

`keep` runs in the foreground. It does not need a daemon, tmux, Overmind,
OpenSSL, or another process manager. macOS and Linux are currently supported.

## Installation

With Homebrew:

```bash
brew install youguanxinqing/tap/keep
```

Or build from source. Requires Rust 1.85+ and
[just](https://github.com/casey/just). Clone the repository, enter the
directory, and run:

```bash
cd keep
just install
keep --version
```

During development, run `just check` for formatting, lints, and all tests.

## Configuration

Configuration lives either in `keep.yaml` at the project's Git root or in
`~/.config/keep/*.yaml`. Auto-detection prefers a local `keep.yaml` and falls
back to matching a global configuration.

Generate a minimal template inside the project directory:

```bash
keep config init                    # writes ~/.config/keep/<project-name>.yaml
keep config init --local            # writes keep.yaml at the current Git root
keep config init --project shop     # sets the project name explicitly
```

The command records the Git remote when one exists, falling back to the local
path, and never overwrites an existing file. After generating, only
`processes.app.command` needs editing.

An explicit `--config` always wins; otherwise the lookup order is the local
`keep.yaml`, then global configurations.

```yaml
version: 1

project:
  name: shop                  # also the runtime ID when no id is set
  git:                        # recommended: one config works across Git worktrees
    - git@github.com:acme/shop.git

env_files:
  - .env

processes:
  database:
    command: docker compose up postgres
    readiness:
      type: tcp
      target: 127.0.0.1:5432
      interval: 500ms
      attempt_timeout: 1s
      startup_timeout: 30s

  migrate:
    command: ./scripts/migrate.sh
    mode: task
    depends_on:
      database: ready

  api:
    command: npm run dev
    color: red                  # optional: highlight important processes
    log_directory: .keep/logs  # optional: append to a file while still printing
    depends_on:
      database: ready
      migrate: completed_successfully
    readiness:
      type: http
      target: http://127.0.0.1:3000/health
      expected_status: 200
      startup_timeout: 30s
    restart:
      policy: on-failure
      max_attempts: 3
```

This configuration means:

1. Start `database` and wait until the TCP port accepts connections.
2. Run `migrate` once and wait for it to exit successfully.
3. Start `api` and wait until the health check returns HTTP 200.

Regular processes default to long-running `service`s; `mode: task` marks a
one-off task that exits successfully. `readiness` also supports `tcp4`,
`tcp6`, `https`, `unix`, `file`, and `command`.

See the [configuration reference](docs/configuration.md) for all fields,
defaults, and constraints.

Check the configuration first:

```bash
keep config validate          # checks the configuration selected for this project
keep config validate --all    # checks all global configurations
keep config resolve            # shows which project the current directory matches
```

## Usage

Start all processes from the project directory. `keep start` stays in the
foreground and aggregates logs; press `Ctrl-C` to stop the whole project:

```bash
cd ~/projects/shop
keep start
```

### Output and logs

Child stdout/stderr streams in real time with a process-name prefix, such as
`api | listening on :3000`. Terminal-aware programs such as Python continue
to flush normally.

Prefixes receive stable terminal colors. Per-process colors, local log files,
and file-only output are configured with `color`, `log_directory`, and
`console`; see [Log colors](docs/configuration.md#log-colors) and
[Logging to disk](docs/configuration.md#logging-to-disk).

You can also pick a configuration explicitly, or start one process and its
dependencies:

```bash
keep start --config shop
keep start api
```

Manage running projects from another terminal, in any directory:

```bash
keep ls                       # list all projects and processes
keep status shop              # show project details
keep status shop/api          # show one process
keep restart shop/api         # restart one process
keep restart api              # project can be omitted when the name is unique
keep stop shop/api            # stop one process
keep stop shop                # stop the whole project
keep wait shop/api            # block until the process is running (default timeout 5 min)
keep wait api -s stopped -t 30  # wait for another state with a custom timeout in seconds

keep stop --all               # stop every project managed by keep
```

Common commands have short aliases: `s` (start), `l` (ls), `ps` (status),
`st` (stop), `r` (restart), `w` (wait), `q` (quit).

`keep restart` and `keep start` re-read the configuration file first, so edits to
a command, its environment, or its readiness probe take effect on the next
restart. Changes that cannot be applied to a live supervisor — a different set of
process names, a renamed or relocated project, a new `log_directory` — are
rejected with an explanation and need `keep quit <project>` plus `keep start`.

## Procfile compatibility mode

Procfiles do not participate in auto-detection and must be run explicitly:

```bash
keep procfile start --file Procfile --project shop
keep procfile convert --file Procfile --project shop > shop.yaml
```

See the [design documents](docs/README.md) for the architecture and roadmap.
