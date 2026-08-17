English | [简体中文](configuration.zh-CN.md)

# keep configuration reference

Native configuration lives either in `keep.yaml` at the Git root or in
`~/.config/keep/*.yaml` / `~/.config/keep/*.yml`. Each file in the global
directory describes one project; `KEEP_CONFIG_DIR` changes the global
configuration directory.

Check the configuration after editing:

```bash
keep config validate
keep config validate --all  # checks all configurations in the global directory
keep config resolve
```

`keep` rejects unknown fields, duplicate effective project IDs, invalid
dependencies, and dependency cycles, so typos are never silently ignored.

## Creating a minimal configuration

```bash
keep config init
keep config init --project shop
keep config init --local
```

By default this writes `~/.config/keep/<project-name>.yaml`. `--local` writes
`keep.yaml` at the current Git root instead, after which `keep start` works
directly. The project name defaults to the Git root or current directory
name and can be set with `--project`; without an explicit `id`, that name
also serves as the runtime ID.

The generator prefers writing normalized, credential-free Git remotes so one
configuration works across worktrees; without a remote it writes the
project's absolute `path`. It refuses to overwrite an existing target file.

The minimal version-1 configuration has only these fields:

```yaml
version: 1
project:
  name: shop
processes:
  app:
    command: npm run dev
```

Every other field is optional. The project name also serves as the runtime ID
used by global `ls`, `stop`, and `status` commands. A process name and command
are the minimum needed to start a process.

## Full structure

The skeleton below lists every version-1 field. Delete any optional field you
do not need.

```yaml
version: 1

project:
  id: shop
  name: Shop
  path: ~/projects/shop
  git:
    - git@github.com:acme/shop.git
  aliases:
    - shop-api

env_files:
  - .env

defaults:
  stop:
    signal: TERM
    timeout: 5s
  restart:
    policy: on-failure
    backoff: 1s
    max_attempts: 5

processes:
  database:
    command: docker compose up postgres
    readiness:
      type: tcp
      target: 127.0.0.1:5432

  migrate:
    command: ./scripts/migrate.sh
    mode: task
    depends_on:
      database: ready

  api:
    command: npm run dev
    mode: service
    color: red
    log_directory: .keep/logs
    console: true
    working_directory: services/api
    env_files:
      - services/api/.env
    env:
      PORT: "3443"
    depends_on:
      database: ready
      migrate: completed_successfully
    readiness:
      type: https
      target: https://127.0.0.1:${PORT}/health
      interval: 1s
      attempt_timeout: 1s
      startup_timeout: 30s
      success_threshold: 1
      method: GET
      headers:
        Authorization: Bearer ${DEV_TOKEN}
      expected_status: 200
      tls_ca: certs/development-ca.pem
    restart:
      policy: on-failure
      backoff: 1s
      max_attempts: 5
    stop:
      signal: TERM
      timeout: 5s
```

## Top-level fields

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `version` | integer | yes | none | Configuration format version; currently must be `1`. |
| `project` | object | yes | none | Project identity and auto-detection rules. |
| `env_files` | string list | no | `[]` | Environment files loaded by every process. |
| `defaults` | object | no | `{}` | Project-wide stop and restart defaults. |
| `processes` | object | yes | none | Map of process name to process configuration; at least one process. |

## `project`

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `id` | string | one of the two | `name` | Optional stable runtime ID — the `shop` in `keep stop shop`. |
| `name` | string | one of the two | `id` | Project name; also the runtime ID when no `id` is set. |
| `path` | string | no | none | Project directory. Supports absolute paths and `~/...`; other relative paths resolve from the home directory. |
| `git` | string list | no | `[]` | Matchable Git remotes. Common SSH and HTTPS forms are normalized before comparison. |
| `aliases` | string list | no | `[]` | Additional project directory names or Git root names. |

At least one of `id` and `name` must be set. The effective runtime ID is
`id ?? name`: an explicit `id` always wins; without one, `name` is limited to
48 characters using only ASCII letters, digits, `_`, and `-`. Duplicate
effective IDs within the same configuration directory are rejected. With only
a `name`, renaming the project changes its identity; set a stable `id` if you
want an independent friendly display name.

Configuration selection order: explicit `--config`, then `keep.yaml` at the
Git root, and only then a scan of the global directory matching
`project.path`, `project.git`, `id`, `name`, or `aliases` in that order. A
local configuration that exists but is invalid is an error, never a silent
fallback. Multiple global candidates at the same tier are an error, never a
guess.

### Git worktrees

To reuse one configuration across worktrees, omit `path` and use `git`:

```yaml
project:
  name: shop
  git:
    - git@github.com:acme/shop.git
```

Run `keep start` in any worktree and the current Git root becomes the project
directory. One effective project ID can currently run only one instance at a
time; starting multiple worktrees in parallel requires distinct IDs.

## `defaults`

### `defaults.stop`

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `signal` | string | no | `TERM` | Signal sent to the whole process group for a graceful stop. |
| `timeout` | duration | no | `5s` | Maximum wait for exit before `KILL` is sent. |

Supported signals: `ABRT`, `HUP`, `INT`, `KILL`, `QUIT`, `STOP`, `TERM`,
`USR1`, `USR2`, with or without a `SIG` prefix such as `SIGTERM`.

### `defaults.restart`

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `policy` | enum | no | `never` | `never`, `on-failure`, or `always`. |
| `backoff` | duration | no | `1s` | Wait between two starts. |
| `max_attempts` | non-negative integer | no | unlimited | Maximum retries, not counting the first start. |

`on-failure` restarts only on a non-zero exit, a signal exit, or a readiness
failure; `always` also restarts a service after a clean exit. A successfully
completed task is never restarted. Once the retry limit is reached, the
process enters the failed state.

A process's own `stop` or `restart` object replaces the corresponding
`defaults` object wholesale — it is not merged field by field. For example, a
process with `restart: { policy: always }` has unlimited `max_attempts`.

## `processes.<name>`

`<name>` is the process name, with the same format rules as effective project
IDs, unique within the project. Declaration order in the YAML stabilizes log
ordering and start order when multiple processes become eligible at once.

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `command` | string | yes | none | Non-empty command executed with POSIX `sh -c`. |
| `mode` | enum | no | `service` | `service` is long-running; `task` is a one-off that exits successfully. |
| `color` | color name or integer | no | auto-assigned | Log prefix color, as a color name or xterm number `0..255`. |
| `log_directory` | string | no | none | Keep terminal output and also append merged stdout/stderr to this directory. |
| `console` | boolean | no | `true` | Whether the process's stdout/stderr also shows in the terminal. |
| `depends_on` | object | no | `{}` | Map of dependency process name to dependency condition. |
| `readiness` | object | no | none | Readiness probe for services; not allowed on a `task`. |
| `restart` | object | no | `defaults.restart` | Per-process restart settings. Same fields as `defaults.restart`. |
| `stop` | object | no | `defaults.stop` | Per-process stop settings. Same fields as `defaults.stop`. |
| `working_directory` | string | no | project root | Command working directory; relative paths resolve from the project root. |
| `env_files` | string list | no | `[]` | Environment files loaded only for this process; relative paths resolve from the project root. |
| `env` | string map | no | `{}` | Environment variables for this process only; values must be strings. |

Commands inherit the system environment `keep` was started with, then
override same-named variables in this order:

1. Top-level `env_files`, loaded in declaration order.
2. Process `env_files`, loaded in declaration order.
3. Process `env`.

A missing or invalid environment file prevents the process from starting.
`command` gets environment expansion from the shell; readiness `target`,
`tls_ca`, and header values support `${NAME}`.

### Log colors

Without a `color` setting, keep cycles through 10 default colors in the
processes' YAML order; explicitly configured colors are skipped in the
automatic palette. Colors are assigned from the full configuration, so a
process keeps the same color when only some processes are started or when a
process restarts. Colors may repeat once the palette is exhausted.

Common colors can be named directly: `red`, `green`, `yellow`, `blue`,
`magenta`, `cyan`. For a closer fit with your terminal theme, use an xterm
color number from `0` to `255`:

```yaml
processes:
  api:
    command: npm run dev
    color: red
  worker:
    command: npm run worker
    color: 208
```

Colors apply only to the `api |` style process-name prefix, never to the log
body. stdout and stderr share the same process color; when output goes to a
file or pipe, keep writes no ANSI escapes for the prefix. To disable terminal
colors temporarily:

```bash
NO_COLOR=1 keep start
```

### Logging to disk

Configure `log_directory` for processes whose logs should be kept locally:

```yaml
processes:
  api:
    command: npm run dev
    log_directory: .keep/logs
```

Terminal output is not turned off; keep also appends stdout and stderr, in
the order received, to `.keep/logs/api.log`. Relative directories resolve
from the project root, absolute directories are used as-is; missing
directories are created automatically. The file contains only the raw bytes
captured from the process — no `api |` prefix and no keep-added colors.
Process restarts and subsequent `keep start` runs keep appending without
overwriting existing content.

Set `console: false` to write to the file only, without showing that
process's output in the terminal:

```yaml
processes:
  api:
    command: npm run dev
    log_directory: .keep/logs
    console: false
```

`console: false` never hides keep's own errors; to prevent silently losing
process output, it requires `log_directory` to be set as well. If writing the
log file fails at runtime, keep reports the error and falls back to the
terminal for subsequent output.

keep does not handle log rotation, compression, or retention; use existing
tools like the system's logrotate to bound disk usage. If the directory
cannot be created or the log file cannot be opened, keep reports an error
before starting the process.

## `depends_on`

```yaml
depends_on:
  database: ready
  migrate: completed_successfully
```

| Condition | Meaning |
| --- | --- |
| `ready` | The dependency passed its readiness probe; without a probe, a successful start counts as ready. |
| `completed_successfully` | The dependency is a `mode: task` and exited with status 0. |

Dependencies must exist, cannot point at the process itself, and cannot form
cycles. `completed_successfully` may only point at a `task`. Running
`keep start api` also starts all of `api`'s transitive dependencies.

## `readiness`

### Common fields

| Field | Type | Required | Default | Description |
| --- | --- | --- | --- | --- |
| `type` | enum | yes | none | `tcp`, `tcp4`, `tcp6`, `http`, `https`, `unix`, `file`, or `command`. |
| `target` | string | yes | none | Probe target, non-empty; format depends on `type`. |
| `interval` | duration | no | `1s` | Wait between two probes. |
| `attempt_timeout` | duration | no | `1s` | Maximum time for one TCP, HTTP(S), or command probe. |
| `startup_timeout` | duration | no | `30s` | Total time limit for the service to pass the probe. |
| `success_threshold` | positive integer | no | `1` | Consecutive successes required to count as ready; a failure resets the count. |
| `method` | string | no | `GET` | HTTP(S) request method. Ignored by other probe types. |
| `headers` | string map | no | `{}` | HTTP(S) request headers; values support `${NAME}`. |
| `expected_status` | integer | no | any `200..299` | The single acceptable HTTP status code, between 100 and 599. |
| `tls_ca` | string | no | system roots | PEM CA file for HTTPS; relative paths resolve from the project root. |

All durations must be greater than zero. humantime formats like `500ms`,
`2s`, and `1m 30s` are supported. A readiness probe that exceeds
`startup_timeout` stops the process; processes depending on it stay blocked.
With a restart policy enabled, readiness failures count toward retries.

With `tls_ca` set, the certificates in that file become the trust root for
this probe — useful for local self-signed certificates. Do not commit secrets
in headers to version control; reference them via environment variables
instead.

### Probe types and targets

| `type` | Example `target` | Success condition |
| --- | --- | --- |
| `tcp` | `127.0.0.1:5432` | Any resolved IPv4 or IPv6 address accepts a connection. `tcp://` prefix allowed. |
| `tcp4` | `localhost:5432` | A resolved IPv4 address accepts a connection. `tcp4://` prefix allowed. |
| `tcp6` | `[::1]:5432` | A resolved IPv6 address accepts a connection. `tcp6://` prefix allowed. |
| `http` | `http://127.0.0.1:3000/health` | Request succeeds with an acceptable status code. |
| `https` | `https://localhost:3443/health` | TLS request succeeds with an acceptable status code. |
| `unix` | `unix:///tmp/shop.sock` | The Unix socket accepts a connection. Absolute paths recommended. |
| `file` | `tmp/ready` | The file exists; relative paths resolve from the project root. `file://` prefix allowed. |
| `command` | `pg_isready -h 127.0.0.1` | Runs with `sh -c` in the project root and exits with status 0. |

Command probes inherit the process's environment, their output is discarded,
and a timeout terminates the whole probe process group.

## Paths, durations, and validation rules

- Relative `project.path` values resolve from the user's home directory.
- Relative paths in `working_directory`, `env_files`, file targets, and `tls_ca` resolve from the project root.
- Unix sockets should always use absolute paths.
- Effective project IDs and process names are at most 48 characters of ASCII letters, digits, `_`, and `-`.
- Durations must be greater than zero and within what the platform timer can represent.
- HTTP status codes must be between 100 and 599.
- Unknown fields, empty commands, empty targets, missing dependencies, self-dependencies, and dependency cycles all fail validation.
