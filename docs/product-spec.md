# Product specification

## Summary

`keep` is a project-aware process supervisor for local development. It combines
the foreground command-line workflow of Overmind with an explicit dependency
graph and readiness probes inspired by dockerize.

The first implementation is a native command-line supervisor written in Rust.
It does not require tmux and does not initially provide a daemon or an
interactive PTY attachment feature.

## Product principles

1. A spawned process is not necessarily a ready service.
2. Process dependencies are explicit, validated, and observable.
3. Starting a project may use the current directory; controlling a running
   project must not depend on the current directory.
4. Configuration, runtime registration, and persistent state are separate.
5. Ambiguous project matching is an error, never a guess.
6. Every user-visible command has an end-to-end test.
7. Procfile support is an explicit compatibility mode, not the native model.

## Configuration location

Native configuration files live in:

```text
~/.config/keep/*.yaml
~/.config/keep/*.yml
```

`KEEP_CONFIG_DIR` may override this location for testing and automation. A
native configuration has a schema version and a globally unique project ID.

## Project discovery

`keep start` resolves a configuration using this order:

1. An explicit `--config <id-or-path>` argument.
2. The current directory contained by `project.path`; the longest matching path
   wins.
3. A normalized Git remote matching an entry in `project.git`.
4. The Git root or current directory basename matching `project.name` or an
   alias.

Ties at the same priority are reported with all candidates. Git URLs are
normalized so common SSH and HTTPS forms for the same host and repository
compare equally.

The diagnostic command `keep config resolve` explains which configuration was
selected and why.

## Global runtime discovery

Every foreground supervisor registers itself in a per-user runtime directory.
The registry contains a small metadata file and a Unix control socket for each
running project. Runtime clients scan and ping these registrations rather than
looking for a socket in the current project directory.

Consequences:

- `keep ls` lists processes from every running project from any directory.
- `keep stop shop` stops the `shop` project from any directory.
- `keep stop shop/api` stops only the `api` process.
- Moving or deleting a configuration after startup does not prevent shutdown.
- A project ID identifies at most one active instance in version 1.

The global process address syntax is `<project-id>/<process-name>`.

## Command-line interface

The version 1 interface is:

```text
keep start [process...]
keep ls [project]
keep status [project-or-process]
keep stop [project-or-process...]
keep stop --all
keep restart [project-or-process...]
keep quit <project>

keep config list
keep config show [project]
keep config resolve
keep config validate [--all]

keep procfile start [--file Procfile]
keep procfile convert [--file Procfile]
keep doctor
```

When `keep stop` has no target, it stops the active project resolved from the
current directory. If no running project can be resolved, it prints the active
projects and requires an explicit target. It must not silently select an
unrelated project.

## Process lifecycle

A process moves through explicit states:

```text
pending -> blocked -> starting -> running -> checking -> ready
                         |           |          |
                         +--------> failed <----+

ready -> stopping -> stopped
ready -> exited -> restarting -> starting
```

The exact state and any blocking or probe failure reason are returned by the
control API and displayed by `keep status`.

Startup behavior:

- Dependencies form a directed acyclic graph.
- Independent branches start concurrently.
- YAML declaration order is the stable tie-breaker for simultaneously runnable
  processes and for log presentation.
- Starting selected processes includes their transitive dependency closure.
- A dependency with no readiness probe becomes ready after it is spawned.
- A task dependency may gate on `completed_successfully`.

Shutdown behavior:

- The whole project stops in reverse dependency order.
- Each process receives its configured graceful stop signal.
- The entire process group is killed after the stop timeout.
- A targeted process operation does not implicitly restart or kill unrelated
  running processes.

## Foreground execution

The built-in supervisor launches commands directly and captures stdout and stderr.
`keep start` stays in the foreground, prefixes output with the process name,
and owns the lifetime of all child process groups. A second terminal controls
the supervisor through its Unix socket.

Version 1 limitations are intentional:

- no tmux backend;
- no native PTY or interactive attachment;
- no daemon mode;
- no persistent log archive;
- no remote TCP control socket;
- no multi-instance scaling;
- no implicit port allocation.

Some child programs disable colored output when stdout is a pipe. `keep` passes
through ANSI sequences that are emitted, but it does not globally force color.

## Procfile compatibility

Procfiles are never selected by native automatic discovery. Compatibility is
entered explicitly with `keep procfile start`. Standard `name: command` entries
are converted to an in-memory native configuration and supervised by the same
runtime.

Initial compatibility includes process execution, aggregated logs, environment
loading, and lifecycle control. Complete compatibility with all Overmind flags,
formation rules, and automatic port environment variables is not a version 1
goal.

## Non-goals

Version 1 is not a production init system, container orchestrator, remote
deployment tool, template renderer, or distributed service manager. Windows
support is not part of the initial macOS and Linux target.
