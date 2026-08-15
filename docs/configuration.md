# Configuration specification

## Example

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
    policy: never

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
    command: cargo run --bin migrate
    mode: task
    depends_on:
      database: ready

  api:
    command: cargo run --bin api
    depends_on:
      database: ready
      migrate: completed_successfully
    readiness:
      type: http
      target: http://127.0.0.1:8080/health
      expected_status: 200
      tls_ca: certs/development-ca.pem
      interval: 500ms
      attempt_timeout: 2s
      startup_timeout: 60s
    restart:
      policy: on-failure
      backoff: 1s
      max_attempts: 5

  worker:
    command: cargo run --bin worker
    depends_on:
      database: ready
      migrate: completed_successfully
```

## Schema rules

- `version` is required. Version 1 readers reject future versions with an
  actionable error.
- `project.id` is required, stable, and unique in the configuration directory.
  It uses ASCII letters, digits, `_`, and `-`.
- `project.name` is a human-readable name and a weak project matcher.
- `project.path` is expanded relative to the user's home directory and then
  normalized.
- `project.git` may contain multiple equivalent repository addresses.
- Process names use the same safe character set as project IDs and are unique
  within the project.
- `command` is executed by the system's POSIX `sh` from the project root unless
  a process-specific working directory is supplied.
- Relative environment files and working directories are resolved from the
  project root.
- Unknown fields are errors so configuration typos cannot be ignored silently.

## Dependency conditions

Version 1 defines:

- `ready`: the dependency has passed readiness, or has spawned when no probe is
  configured.
- `completed_successfully`: a task exited with status 0.

The parser rejects missing dependencies, self-dependencies, dependency cycles,
and `completed_successfully` targeting a service process.

## Readiness probes

The supported probe types are `tcp`, `tcp4`, `tcp6`, `http`, `https`, `unix`,
`file`, and `command`.

Every probe has independent retry timing:

- `interval`: delay between attempts;
- `attempt_timeout`: timeout of one attempt;
- `startup_timeout`: total time allowed to become ready;
- `success_threshold`: consecutive successes required, defaulting to one.

`attempt_timeout` bounds TCP, HTTP(S), and command attempts. File probes and
Unix socket connects are kernel-local checks that normally return immediately;
`startup_timeout` still bounds their retry loop. DNS resolution uses the system
resolver and cannot be interrupted portably, so a slow resolver may overrun one
TCP or HTTP attempt.

HTTP probes may configure method, headers, and expected status. HTTPS probes
may additionally configure a project-relative or absolute `tls_ca` PEM file for
private development certificate authorities. Secrets in headers must never be
printed by validation, status, or probe diagnostics.

## Versioning

Configuration is parsed into a strict, source-aware version 1 model before any
process starts. Durations, identifiers, dependency references, and cycles are
validated at this boundary. A separate migration model can be introduced when
a second schema version actually exists.
