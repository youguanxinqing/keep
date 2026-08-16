# Architecture

## Overview

```text
keep start
  -> config repository
  -> project resolver
  -> validated configuration
  -> dependency graph
  -> foreground supervisor
       |- direct process backend
       |- readiness engine
       |- output multiplexer
       `- Unix control server
              |
              `-> per-user runtime registry
                         ^
                         |
                 keep ls/stop/status/restart
```

The current directory is an input to project resolution, not part of the
runtime addressing model.

`keep` ships as one self-contained binary. The supervisor is an internal module,
not an external service or package. Runtime commands require a POSIX `sh`; Git is
optional and used only for repository discovery. HTTPS probing is compiled in
with Rustls and does not require OpenSSL.

## Components

### Configuration repository

Loads repository-local `keep.yaml` before scanning the global configuration
directory, parses YAML, derives `project.id` from `project.name` when omitted,
validates unique effective project IDs, and produces source-aware diagnostics.
Downstream modules always receive a validated ID.

### Project resolver

Collects the canonical current directory, Git root, normalized remotes, and
directory basenames. It returns either one configuration with matching evidence
or an ambiguity/not-found diagnostic.

### Dependency graph

Validates references and cycles, computes transitive dependency closures, emits
runnable batches, and computes reverse shutdown order. Graph logic is pure and
does not spawn processes.

### Supervisor

Owns all mutable runtime state. Commands from signals, child exits, probes, and
the control server enter one event loop. This avoids several subsystems racing
to update process state.

### Direct process backend

Starts each command in a separate Unix process group. stdout and stderr use
separate output-only native pseudo-terminals so terminal-aware programs retain
normal flushing while the streams remain distinguishable. It exposes process
events, output, group signaling, and exit status. Graceful stop signals target
the group, followed by `SIGKILL` after the configured timeout. The supervisor
reaps every direct process leader and does not mark stop complete while members
remain in its process group; descendants left behind by a completed leader are
killed before output is drained.

### Readiness engine

Runs cancellable, asynchronous probe attempts and reports transitions to the
supervisor. Probe tasks never directly start dependent processes.

### Output multiplexer

Reads stdout and stderr without blocking the supervisor, preserves emitted ANSI
bytes, and prefixes complete lines. Reader threads send lines through one bounded
queue to a single writer; shutdown closes the queue and waits until pending output
has been written. A slow control client must not block child output.

### Runtime registry

Each supervisor owns one directory containing metadata and a Unix socket. The
directory name is derived from the stable project ID. An OS file lock, held for
the supervisor's full lifetime, provides atomic single-instance ownership.

Metadata contains protocol version, project ID/name/root, configuration path,
supervisor PID, start time, and socket path. It never contains environment
values or secrets.

Clients verify registrations by pinging the socket. An unreachable registration
may be cleaned only after acquiring its project lock; this remains correct when
a PID has been reused. If the lock is still held, the supervisor is reported as
unresponsive and its files are not removed.

Runtime directory and socket permissions are `0700` and `0600`. Linux prefers
`$XDG_RUNTIME_DIR/keep`. Other Unix platforms use a short, user-owned runtime
path to remain below Unix socket path-length limits.

### Control protocol

The local protocol uses newline-framed, versioned JSON messages. Version 1
requests are `ping`, `status`, `start_processes`, `stop_processes`,
`restart_processes`, and `shutdown`.

Connections have bounded reads and writes, and requests are limited to 16 KiB.
Mutating clients wait for the configured lifecycle operation rather than using a
fixed timeout shorter than a valid stop timeout.

Transport and request models remain separate so protocol compatibility can be
tested without starting real child processes.

## Failure behavior

- Invalid configuration prevents any child from starting.
- A readiness timeout marks the process failed and leaves dependents blocked
  with a visible reason.
- A critical service exit follows its restart policy and otherwise initiates
  project shutdown.
- `SIGINT`, `SIGTERM`, and terminal hangup initiate reverse-order shutdown.
- Unexpected supervisor death may leave a stale registration; global clients
  detect it rather than treating metadata as authoritative.

## Backend evolution

Version 1 keeps direct process execution and output-only pseudo-terminals inside
the supervisor. A backend interface should be extracted only if full interactive
PTY or tmux execution is implemented; those backends may not alter configuration,
dependency, registry, or control protocol semantics.
