# Roadmap

## Milestone 0: repository foundation

- [x] Rust package and `justfile`
- [x] Initial CLI command tree and stable exit-error formatting
- [x] Configuration directory override for isolated tests
- [x] Documentation index and contribution test policy
- [x] End-to-end tests for every currently exposed foundation command

## Milestone 1: configuration and project resolution

- [x] Version 1 strict YAML parser and validated runtime model
- [x] Strict unknown-field validation
- [x] Configuration list/show/validate commands
- [x] Path, project name, alias, and Git remote resolution
- [x] Explainable matching and ambiguity diagnostics
- [x] Dependency reference and cycle validation
- [x] End-to-end coverage for all currently exposed configuration commands

## Milestone 2: foreground direct supervisor

- [x] Direct command execution in Unix process groups
- [x] stdout/stderr aggregation and process-name prefixes
- [x] Lifecycle state machine
- [x] Signal forwarding and graceful/forced shutdown
- [x] Dependency scheduling and reverse shutdown
- [x] `keep start` end-to-end dependency and full signal tests

## Milestone 3: global runtime control

- [x] Per-user runtime directory and atomic registration
- [x] Versioned Unix control protocol
- [x] `keep ls` across multiple projects
- [x] Global project/process target parsing
- [x] `stop`, `restart`, `status`, and `quit`
- [x] Stale and unresponsive registration handling
- [x] End-to-end tests from directories unrelated to every running project

## Milestone 4: readiness

- [x] TCP, TCP4, and TCP6 probes
- [x] HTTP and HTTPS probes
- [x] Unix socket and file probes
- [x] Command probes
- [x] Per-probe retry, attempt timeout, startup timeout, and success threshold
- [x] Blocked and failed dependency diagnostics
- [x] End-to-end tests using real local listeners and fixture commands

## Milestone 5: Procfile compatibility

- [x] Explicit `procfile start`
- [x] Procfile parser and environment loading
- [x] `procfile convert`
- [x] End-to-end compatibility and conversion tests

## Later milestones

- daemon mode and log subscription/history;
- native PTY support;
- optional tmux backend;
- multiple instances and process scaling;
- shell completions and cross-platform release packaging.
