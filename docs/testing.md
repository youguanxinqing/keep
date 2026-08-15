# Testing strategy

## Non-negotiable policy

Every user-visible command or option must have at least one end-to-end test that
executes the compiled `keep` binary. A feature is not complete when it only has
unit tests.

The end-to-end suite is organized by command so omissions are visible during
review:

```text
tests/e2e_config.rs
tests/e2e_runtime.rs
tests/e2e_control.rs
tests/e2e_lifecycle.rs
tests/e2e_readiness.rs
tests/e2e_compat.rs
```

Each new public subcommand, flag, output contract, and important error path must
be added to the corresponding command test. The roadmap checklist links a
feature to its required end-to-end coverage.

## Test layers

### Unit tests

Unit tests cover pure or narrowly scoped behavior:

- schema parsing and source-aware errors;
- project ID and process name validation;
- path and Git URL normalization;
- resolver ranking and ambiguity;
- dependency cycle detection and topological batches;
- dependency closure and reverse shutdown order;
- lifecycle state transitions and restart backoff;
- control request serialization and protocol versions;
- output prefixing and partial lines;
- probe result classification.

### Integration tests

Component integration tests use temporary directories, local TCP listeners,
local HTTP servers, Unix sockets, and small fixture child programs. They cover:

- registry creation, discovery, stale cleanup, and single-instance locking;
- process-group signaling and forced termination;
- readiness success, retries, timeout, and cancellation;
- successful TCP/TCP4/TCP6, HTTP/HTTPS, Unix socket, file, and command probes;
- control commands racing with process exit;
- environment and working-directory behavior.

### End-to-end tests

End-to-end tests invoke the compiled executable and only observe public output,
exit status, signals, files, and sockets. They use `KEEP_CONFIG_DIR` and a
test-only runtime-directory override so they never touch the developer's real
configuration or active keep instances.

Required scenarios include:

- start a dependency chain and prove ordering from externally written markers;
- start independent branches and prove both can proceed;
- show all processes from two supervisors with `keep ls` from an unrelated
  directory;
- stop a project and an individual process from an unrelated directory;
- restart one process and observe a new PID without restarting siblings;
- display a blocked dependency and readiness diagnostic;
- forward graceful signals, then force-kill a process that ignores them;
- reject duplicate IDs, dependency cycles, unknown fields, and ambiguous
  project matches;
- run and convert a Procfile only through explicit compatibility commands;
- run `doctor` against both valid and invalid installations.

End-to-end tests must bound every wait with a timeout, print captured supervisor
logs on failure, and clean up child processes even when assertions fail.

## Test task contract

The justfile exposes:

```text
just test       # all Rust tests
just test-unit  # library unit tests
just test-e2e   # compiled-binary end-to-end tests
just check      # format check, lints, and all tests
```

`just check` is the CI-ready entry point. Tests that require an unavailable
operating-system feature must report an explicit skip reason; core command tests
may not silently skip.
