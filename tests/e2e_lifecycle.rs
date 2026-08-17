use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

struct Running {
    child: Child,
    project: String,
    config: PathBuf,
    runtime: PathBuf,
    cwd: PathBuf,
    log: PathBuf,
}

impl Running {
    fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = keep(
                &self.config,
                &self.runtime,
                &self.cwd,
                &["stop", &self.project],
            );
        }
        if !wait_for(Duration::from_secs(2), || {
            self.child.try_wait().ok().flatten().is_some()
        }) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn keep(config: &Path, runtime: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(args)
        .env("KEEP_CONFIG_DIR", config)
        .env("KEEP_RUNTIME_DIR", runtime)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn spawn(config: &Path, runtime: &Path, cwd: &Path, project: &str) -> Running {
    let log = cwd.join(format!("{project}-lifecycle.log"));
    let stdout = File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", project])
        .env("KEEP_CONFIG_DIR", config)
        .env("KEEP_RUNTIME_DIR", runtime)
        .current_dir(cwd)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    Running {
        child,
        project: project.into(),
        config: config.into(),
        runtime: runtime.into(),
        cwd: cwd.into(),
        log,
    }
}

fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        thread::sleep(Duration::from_millis(40));
    }
    false
}

fn read_pid(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn process_is_gone(pid: i32) -> bool {
    matches!(signal::kill(Pid::from_raw(pid), None), Err(Errno::ESRCH))
}

#[test]
fn project_and_process_environment_files_are_merged_with_inline_values() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let output_file = root.path().join("environment.txt");
    fs::write(
        root.path().join(".env"),
        "PROJECT=project\nOVERRIDE=project\n",
    )
    .unwrap();
    fs::write(
        root.path().join("process.env"),
        "PROCESS=process\nOVERRIDE=process\n",
    )
    .unwrap();
    fs::write(
        config.path().join("environment.yaml"),
        format!(
            r#"
version: 1
project:
  id: environment
  path: {}
env_files: [.env]
processes:
  write:
    mode: task
    command: "printf '%s|%s|%s|%s' \"$PROJECT\" \"$PROCESS\" \"$OVERRIDE\" \"$INLINE\" > '{}'"
    env_files: [process.env]
    env:
      OVERRIDE: inline
      INLINE: value
"#,
            root.path().display(),
            output_file.display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "environment"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(output_file).unwrap(),
        "project|process|inline|value"
    );
}

#[test]
fn on_failure_policy_restarts_with_backoff_and_exposes_the_count() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let counter = root.path().join("attempts");
    fs::write(
        config.path().join("restartable.yaml"),
        format!(
            r#"
version: 1
project:
  id: restartable
  path: {}
processes:
  api:
    command: |
      count=$(cat '{}' 2>/dev/null || echo 0)
      count=$((count + 1))
      echo "$count" > '{}'
      if [ "$count" -eq 1 ]; then exit 9; fi
      trap 'exit 0' TERM INT HUP
      while :; do sleep 1; done
    restart:
      policy: on-failure
      backoff: 50ms
      max_attempts: 2
"#,
            root.path().display(),
            counter.display(),
            counter.display()
        ),
    )
    .unwrap();
    let running = spawn(
        config.path(),
        runtime.path(),
        unrelated.path(),
        "restartable",
    );
    assert!(
        wait_for(Duration::from_secs(5), || fs::read_to_string(&counter)
            .is_ok_and(|value| value.trim() == "2")),
        "{}",
        running.logs()
    );
    let status = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["status", "restartable/api"],
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.lines().any(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.get(3) == Some(&"running") && fields.get(5) == Some(&"1")
        }),
        "{stdout}\n{}",
        running.logs()
    );
}

#[test]
fn restart_limit_stops_an_endless_crash_loop() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let counter = root.path().join("attempts");
    fs::write(
        config.path().join("crash-loop.yaml"),
        format!(
            r#"
version: 1
project:
  id: crash-loop
  path: {}
processes:
  api:
    command: "echo attempt >> '{}'; exit 5"
    restart:
      policy: always
      backoff: 25ms
      max_attempts: 1
"#,
            root.path().display(),
            counter.display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "crash-loop"],
    );
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(counter).unwrap(), "attempt\nattempt\n");
}

#[test]
fn configured_timeout_force_kills_a_process_that_ignores_term() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    fs::write(
        config.path().join("stubborn.yaml"),
        format!(
            r#"
version: 1
project:
  id: stubborn
  path: {}
defaults:
  stop:
    signal: TERM
    timeout: 100ms
processes:
  api:
    command: "trap '' TERM; while :; do sleep 1; done"
"#,
            root.path().display()
        ),
    )
    .unwrap();
    let mut running = spawn(config.path(), runtime.path(), unrelated.path(), "stubborn");
    assert!(wait_for(Duration::from_secs(5), || {
        keep(
            config.path(),
            runtime.path(),
            unrelated.path(),
            &["ls", "stubborn"],
        )
        .status
        .success()
    }));
    let started = Instant::now();
    let stop = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["stop", "stubborn"],
    );
    assert!(stop.status.success(), "{}", running.logs());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(wait_for(Duration::from_secs(2), || running
        .child
        .try_wait()
        .ok()
        .flatten()
        .is_some()));
}

#[test]
fn stop_removes_descendants_after_the_process_leader_exits() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let leader_pid = root.path().join("leader.pid");
    let descendant_pid = root.path().join("descendant.pid");
    fs::write(
        config.path().join("stop-descendants.yaml"),
        format!(
            r#"
version: 1
project:
  id: stop-descendants
  path: {}
defaults:
  stop:
    timeout: 100ms
processes:
  api:
    command: |
      echo $$ > '{}'
      sh -c 'trap "" TERM; echo $$ > "{}"; while :; do sleep 1; done' &
      trap 'exit 0' TERM
      while :; do sleep 1; done
"#,
            root.path().display(),
            leader_pid.display(),
            descendant_pid.display()
        ),
    )
    .unwrap();
    let mut running = spawn(
        config.path(),
        runtime.path(),
        unrelated.path(),
        "stop-descendants",
    );
    assert!(wait_for(Duration::from_secs(2), || {
        leader_pid.is_file() && descendant_pid.is_file()
    }));
    let group = read_pid(&leader_pid).unwrap();
    let descendant = read_pid(&descendant_pid).unwrap();

    let stop = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["stop", "stop-descendants"],
    );
    let supervisor_exited = wait_for(Duration::from_secs(2), || {
        running.child.try_wait().ok().flatten().is_some()
    });
    let descendant_gone = wait_for(Duration::from_secs(2), || process_is_gone(descendant));
    if !supervisor_exited || !descendant_gone {
        let _ = signal::killpg(Pid::from_raw(group), Signal::SIGKILL);
    }

    assert!(stop.status.success(), "{}", running.logs());
    assert!(supervisor_exited, "{}", running.logs());
    assert!(
        process_is_gone(group),
        "keep stop did not reap process leader {group}\n{}",
        running.logs()
    );
    assert!(
        descendant_gone,
        "keep stop left descendant {descendant} running\n{}",
        running.logs()
    );
}

#[test]
fn interrupting_the_supervisor_gracefully_stops_child_process_groups() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let marker = root.path().join("stopped");
    let leader_pid = root.path().join("leader.pid");
    let descendant_pid = root.path().join("descendant.pid");
    fs::write(
        config.path().join("signals.yaml"),
        format!(
            r#"
version: 1
project:
  id: signals
  path: {}
defaults:
  stop:
    timeout: 100ms
processes:
  api:
    command: |
      echo $$ > '{}'
      sh -c 'trap "" TERM; echo $$ > "{}"; while :; do sleep 1; done' &
      trap 'touch {}; exit 0' TERM
      while :; do sleep 1; done
"#,
            root.path().display(),
            leader_pid.display(),
            descendant_pid.display(),
            marker.display()
        ),
    )
    .unwrap();
    let mut running = spawn(config.path(), runtime.path(), unrelated.path(), "signals");
    assert!(wait_for(Duration::from_secs(5), || {
        keep(
            config.path(),
            runtime.path(),
            unrelated.path(),
            &["ls", "signals"],
        )
        .status
        .success()
    }));
    assert!(wait_for(Duration::from_secs(2), || {
        leader_pid.is_file() && descendant_pid.is_file()
    }));
    let group = read_pid(&leader_pid).unwrap();
    let descendant = read_pid(&descendant_pid).unwrap();
    signal::kill(Pid::from_raw(running.child.id() as i32), Signal::SIGINT).unwrap();
    let supervisor_exited = wait_for(Duration::from_secs(2), || {
        running.child.try_wait().ok().flatten().is_some()
    });
    let descendant_gone = wait_for(Duration::from_secs(2), || process_is_gone(descendant));
    if !supervisor_exited || !descendant_gone {
        let _ = signal::killpg(Pid::from_raw(group), Signal::SIGKILL);
    }
    assert!(supervisor_exited, "{}", running.logs());
    assert!(marker.is_file(), "{}", running.logs());
    assert!(
        process_is_gone(group),
        "Ctrl-C did not reap process leader {group}\n{}",
        running.logs()
    );
    assert!(
        descendant_gone,
        "Ctrl-C left descendant {descendant} running\n{}",
        running.logs()
    );
}

#[test]
fn a_later_spawn_failure_cleans_up_processes_that_already_started() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let pid_file = root.path().join("started.pid");
    fs::write(
        config.path().join("partial.yaml"),
        format!(
            r#"
version: 1
project:
  id: partial
  path: {}
processes:
  started:
    command: "echo $$ > '{}'; trap '' TERM; while :; do sleep 1; done"
    readiness:
      type: file
      target: '{}'
      interval: 10ms
  broken:
    command: "echo unreachable"
    working_directory: missing-directory
    depends_on:
      started: ready
"#,
            root.path().display(),
            pid_file.display(),
            pid_file.display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "partial"],
    );
    assert!(!output.status.success());
    let pid = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    let stopped = wait_for(Duration::from_secs(2), || {
        signal::kill(Pid::from_raw(pid), None).is_err()
    });
    if !stopped {
        let _ = signal::killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
    assert!(stopped, "spawn failure left process group {pid} running");
}

#[test]
fn stop_waits_for_a_configured_timeout_longer_than_five_seconds() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    fs::write(
        config.path().join("slow-stop.yaml"),
        format!(
            r#"
version: 1
project:
  id: slow-stop
  path: {}
defaults:
  stop:
    timeout: 5200ms
processes:
  api:
    command: "trap '' TERM; while :; do sleep 1; done"
"#,
            root.path().display()
        ),
    )
    .unwrap();
    let running = spawn(config.path(), runtime.path(), unrelated.path(), "slow-stop");
    assert!(wait_for(Duration::from_secs(5), || {
        keep(
            config.path(),
            runtime.path(),
            unrelated.path(),
            &["status", "slow-stop"],
        )
        .status
        .success()
    }));

    let output = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["stop", "slow-stop"],
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        running.logs()
    );
}

#[test]
fn a_half_open_control_connection_does_not_freeze_the_supervisor() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    fs::write(
        config.path().join("responsive.yaml"),
        format!(
            r#"
version: 1
project:
  id: responsive
  path: {}
processes:
  api:
    command: "trap 'exit 0' TERM; while :; do sleep 1; done"
"#,
            root.path().display()
        ),
    )
    .unwrap();
    let running = spawn(
        config.path(),
        runtime.path(),
        unrelated.path(),
        "responsive",
    );
    let socket = runtime.path().join("responsive/control.sock");
    assert!(wait_for(Duration::from_secs(5), || socket.exists()));
    let stalled = UnixStream::connect(socket).unwrap();
    thread::sleep(Duration::from_millis(100));

    let mut status = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["status", "responsive"])
        .env("KEEP_CONFIG_DIR", config.path())
        .env("KEEP_RUNTIME_DIR", runtime.path())
        .current_dir(unrelated.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let finished = wait_for(Duration::from_secs(2), || {
        status.try_wait().ok().flatten().is_some()
    });
    drop(stalled);
    if !finished {
        let _ = status.wait();
    }
    assert!(finished, "{}", running.logs());

    let mut oversized =
        UnixStream::connect(runtime.path().join("responsive/control.sock")).unwrap();
    let mut request = vec![b'x'; 16 * 1024 + 1];
    request[16 * 1024] = b'\n';
    let write_rejected = oversized.write_all(&request).is_err();
    let mut response = String::new();
    let _ = oversized.read_to_string(&mut response);
    assert!(
        write_rejected || response.is_empty() || response.contains("control request exceeds"),
        "{response}"
    );
    assert!(keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["status", "responsive"],
    )
    .status
    .success());
}

#[test]
fn output_forwarding_survives_non_utf8_child_bytes() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        config.path().join("bytes.yaml"),
        format!(
            r#"
version: 1
project:
  id: bytes
  path: {}
processes:
  binary:
    mode: task
    command: "printf '\\377first\\nafter\\n'"
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "bytes"],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output
            .stdout
            .windows(b"binary | after".len())
            .any(|window| window == b"binary | after"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn log_directory_combines_stdout_and_stderr_while_preserving_console_output() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        config.path().join("tee.yaml"),
        format!(
            r#"
version: 1
project:
  id: tee
  path: {}
processes:
  app:
    mode: task
    log_directory: logs
    command: |
      printf 'stdout-line\n'
      printf 'stderr-line\n' >&2
"#,
            root.path().display()
        ),
    )
    .unwrap();

    for _ in 0..2 {
        let output = keep(
            config.path(),
            runtime.path(),
            root.path(),
            &["start", "--config", "tee"],
        );
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output
            .stdout
            .windows(b"app | stdout-line".len())
            .any(|line| line == b"app | stdout-line"));
        assert!(output
            .stderr
            .windows(b"app | stderr-line".len())
            .any(|line| line == b"app | stderr-line"));
    }

    let log = fs::read(root.path().join("logs/app.log")).unwrap();
    assert_eq!(
        log.windows(b"stdout-line".len())
            .filter(|line| *line == b"stdout-line")
            .count(),
        2
    );
    assert_eq!(
        log.windows(b"stderr-line".len())
            .filter(|line| *line == b"stderr-line")
            .count(),
        2
    );
    assert!(!log.windows(6).any(|line| line == b"app | "));
    assert!(!root.path().join("logs/app.stdout.log").exists());
    assert!(!root.path().join("logs/app.stderr.log").exists());
}

#[test]
fn console_false_writes_only_the_combined_process_log() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        config.path().join("quiet.yaml"),
        format!(
            r#"
version: 1
project:
  id: quiet
  path: {}
processes:
  app:
    mode: task
    console: false
    log_directory: logs
    command: |
      printf 'stdout-line\n'
      printf 'stderr-line\n' >&2
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "quiet"],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.windows(11).any(|line| line == b"stdout-line"));
    assert!(!output.stderr.windows(11).any(|line| line == b"stderr-line"));
    let log = fs::read(root.path().join("logs/app.log")).unwrap();
    assert!(log.windows(11).any(|line| line == b"stdout-line"));
    assert!(log.windows(11).any(|line| line == b"stderr-line"));
}

#[test]
fn an_unusable_log_directory_prevents_the_process_from_starting() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let marker = root.path().join("started");
    fs::write(root.path().join("not-directory"), "not a directory").unwrap();
    fs::write(
        config.path().join("blocked.yaml"),
        format!(
            r#"
version: 1
project:
  id: blocked
  path: {}
processes:
  app:
    mode: task
    log_directory: not-directory/logs
    command: "touch '{}'"
"#,
            root.path().display(),
            marker.display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "blocked"],
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot prepare log directory"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!marker.exists(), "process started before logs were ready");
}

#[test]
fn short_task_drains_all_output_before_keep_exits() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        config.path().join("burst.yaml"),
        format!(
            r#"
version: 1
project:
  id: burst
  path: {}
processes:
  burst:
    mode: task
    command: "i=1; while [ $i -le 20000 ]; do echo line-$i; i=$((i + 1)); done"
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "burst"],
    );

    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"burst | line-"))
        .count();
    assert_eq!(lines, 20_000, "last output lines were lost");
}

#[test]
fn completed_task_kills_descendants_before_keep_exits() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let leader_pid = root.path().join("leader.pid");
    let descendant_pid = root.path().join("descendant.pid");
    fs::write(
        config.path().join("task-descendant.yaml"),
        format!(
            r#"
version: 1
project:
  id: task-descendant
  path: {}
processes:
  spawn:
    mode: task
    command: "echo $$ > '{}'; sleep 30 & echo $! > '{}'"
"#,
            root.path().display(),
            leader_pid.display(),
            descendant_pid.display()
        ),
    )
    .unwrap();

    let mut supervisor = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", "task-descendant"])
        .env("KEEP_CONFIG_DIR", config.path())
        .env("KEEP_RUNTIME_DIR", runtime.path())
        .current_dir(root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_for(Duration::from_secs(2), || {
        leader_pid.is_file() && descendant_pid.is_file()
    }));
    let leader = read_pid(&leader_pid).unwrap();
    let pid = read_pid(&descendant_pid).unwrap();
    let exited = wait_for(Duration::from_secs(2), || {
        supervisor.try_wait().ok().flatten().is_some()
    });
    if !exited {
        let _ = signal::kill(Pid::from_raw(pid), Signal::SIGKILL);
        let _ = supervisor.kill();
        let _ = supervisor.wait();
    }

    assert!(
        exited,
        "keep waited for a completed task's descendant {pid}"
    );
    assert!(
        process_is_gone(leader),
        "completed task left process leader {leader} unreaped"
    );
    assert!(
        wait_for(Duration::from_secs(2), || process_is_gone(pid)),
        "completed task left descendant {pid} running"
    );
}

#[test]
fn default_service_streams_output_from_terminal_aware_programs() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    fs::write(
        config.path().join("terminal-aware.yaml"),
        format!(
            r#"
version: 1
project:
  id: terminal-aware
  path: {}
processes:
  app:
    command: |
      if [ -t 1 ]; then
        echo visible-without-extra-flags
      fi
      trap 'exit 0' TERM INT HUP
      while :; do sleep 1; done
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let running = spawn(
        config.path(),
        runtime.path(),
        unrelated.path(),
        "terminal-aware",
    );

    assert!(
        wait_for(Duration::from_secs(5), || running
            .logs()
            .contains("app | visible-without-extra-flags")),
        "{}",
        running.logs()
    );
}

#[test]
fn process_working_directory_is_relative_to_the_project_root() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let nested = root.path().join("services/api");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        config.path().join("working-directory.yaml"),
        format!(
            r#"
version: 1
project:
  id: working-directory
  path: {}
processes:
  pwd:
    mode: task
    working_directory: services/api
    command: "pwd > actual-directory"
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        root.path(),
        &["start", "--config", "working-directory"],
    );
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(nested.join("actual-directory"))
            .unwrap()
            .trim(),
        nested.canonicalize().unwrap().to_string_lossy()
    );
}
