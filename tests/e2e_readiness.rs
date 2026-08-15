use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use tempfile::TempDir;

struct Guard {
    child: Child,
    project: String,
    config: PathBuf,
    runtime: PathBuf,
    cwd: PathBuf,
    log: PathBuf,
}

impl Guard {
    fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Guard {
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
        .expect("keep command")
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

fn service_command(extra: &str) -> String {
    format!(
        r#"      {extra}
      trap 'exit 0' TERM INT HUP
      while :; do sleep 1; done"#
    )
}

#[test]
fn all_local_readiness_types_gate_dependents_and_are_visible_in_status() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp4 = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp6 = TcpListener::bind("[::1]:0").unwrap();
    let http = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_address = http.local_addr().unwrap();
    let http_thread = thread::spawn(move || {
        let (mut stream, _) = http.accept().expect("HTTP probe connection");
        let mut request = [0_u8; 2048];
        let size = stream.read(&mut request).unwrap();
        assert!(String::from_utf8_lossy(&request[..size]).contains("X-Keep-Test: yes"));
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let https = TcpListener::bind("127.0.0.1:0").unwrap();
    let https_address = https.local_addr().unwrap();
    let mut certificate_reader =
        BufReader::new(include_bytes!("fixtures/localhost-cert.pem").as_slice());
    let certificates = rustls_pemfile::certs(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut key_reader = BufReader::new(include_bytes!("fixtures/localhost-key.pem").as_slice());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .unwrap()
        .unwrap();
    let tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .unwrap();
    let https_thread = thread::spawn(move || {
        let (socket, _) = https.accept().expect("HTTPS probe connection");
        let connection = rustls::ServerConnection::new(std::sync::Arc::new(tls_config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, socket);
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
    });
    let unix_path = root.path().join("ready.sock");
    let _unix = UnixListener::bind(&unix_path).unwrap();
    let delayed_file = root.path().join("ready.flag");
    let threshold_count = root.path().join("threshold.count");
    let delayed_file_writer = delayed_file.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(700));
        fs::write(delayed_file_writer, "ready").unwrap();
    });
    let final_marker = root.path().join("final.started");
    fs::write(
        root.path().join(".env"),
        format!("TCP_PORT={}\n", tcp.local_addr().unwrap().port()),
    )
    .unwrap();
    fs::write(
        root.path().join("test-ca.pem"),
        include_bytes!("fixtures/test-ca.pem"),
    )
    .unwrap();

    let long = service_command(":");
    let final_command = service_command(&format!("touch '{}'", final_marker.display()));
    fs::write(
        config_dir.path().join("ready.yaml"),
        format!(
            r#"
version: 1
project:
  id: ready
  path: {}
env_files: [.env]
processes:
  tcp:
    command: |
{long}
    readiness:
      type: tcp
      target: "127.0.0.1:${{TCP_PORT}}"
      interval: 50ms
      startup_timeout: 3s
  tcp4:
    command: |
{long}
    readiness:
      type: tcp4
      target: "{}"
  tcp6:
    command: |
{long}
    readiness:
      type: tcp6
      target: "{}"
  http:
    command: |
{long}
    readiness:
      type: http
      target: "http://{http_address}/health"
      expected_status: 204
      headers:
        X-Keep-Test: yes
  https:
    command: |
{long}
    readiness:
      type: https
      target: "https://localhost:{}/health"
      expected_status: 200
      tls_ca: test-ca.pem
  unix:
    command: |
{long}
    readiness:
      type: unix
      target: "{}"
  file:
    command: |
{long}
    readiness:
      type: file
      target: "{}"
      interval: 50ms
      startup_timeout: 3s
  command:
    command: |
{long}
    readiness:
      type: command
      target: "test -f '{}'"
      interval: 50ms
      startup_timeout: 3s
  threshold:
    command: |
{long}
    readiness:
      type: command
      target: "value=$(cat '{}' 2>/dev/null || echo 0); echo $((value + 1)) > '{}'"
      interval: 25ms
      success_threshold: 2
  final:
    command: |
{final_command}
    depends_on:
      tcp: ready
      tcp4: ready
      tcp6: ready
      http: ready
      https: ready
      unix: ready
      file: ready
      command: ready
      threshold: ready
"#,
            root.path().display(),
            tcp4.local_addr().unwrap(),
            tcp6.local_addr().unwrap(),
            https_address.port(),
            unix_path.display(),
            delayed_file.display(),
            delayed_file.display(),
            threshold_count.display(),
            threshold_count.display(),
        ),
    )
    .unwrap();
    let log = unrelated.path().join("readiness.log");
    let stdout = File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", "ready"])
        .env("KEEP_CONFIG_DIR", config_dir.path())
        .env("KEEP_RUNTIME_DIR", runtime_dir.path())
        .current_dir(unrelated.path())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let guard = Guard {
        child,
        project: "ready".into(),
        config: config_dir.path().into(),
        runtime: runtime_dir.path().into(),
        cwd: unrelated.path().into(),
        log,
    };

    assert!(
        wait_for(Duration::from_secs(3), || {
            let output = keep(
                config_dir.path(),
                runtime_dir.path(),
                unrelated.path(),
                &["status", "ready/final"],
            );
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("\tblocked\t")
        }),
        "{}",
        guard.logs()
    );
    assert!(
        wait_for(Duration::from_secs(8), || final_marker.is_file()),
        "{}",
        guard.logs()
    );
    let status = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["status", "ready"],
    );
    let stdout = String::from_utf8_lossy(&status.stdout);
    for process in [
        "tcp",
        "tcp4",
        "tcp6",
        "http",
        "https",
        "unix",
        "file",
        "command",
        "threshold",
        "final",
    ] {
        assert!(stdout.contains(&format!("ready\t{process}\t")), "{stdout}");
    }
    assert!(!stdout.contains("\tchecking\t"), "{stdout}");
    assert_eq!(fs::read_to_string(threshold_count).unwrap(), "2\n");
    http_thread.join().unwrap();
    https_thread.join().unwrap();
}

#[test]
fn readiness_timeout_fails_start_and_keeps_dependents_blocked() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    fs::write(
        config_dir.path().join("failure.yaml"),
        format!(
            r#"
version: 1
project:
  id: failure
  path: {}
processes:
  service:
    command: "trap 'exit 0' TERM; while :; do sleep 1; done"
    readiness:
      type: file
      target: missing.flag
      interval: 25ms
      startup_timeout: 150ms
  downstream:
    command: "echo should-not-run"
    depends_on:
      service: ready
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let log = unrelated.path().join("failure.log");
    let stdout = File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", "failure"])
        .env("KEEP_CONFIG_DIR", config_dir.path())
        .env("KEEP_RUNTIME_DIR", runtime_dir.path())
        .current_dir(unrelated.path())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let guard = Guard {
        child,
        project: "failure".into(),
        config: config_dir.path().into(),
        runtime: runtime_dir.path().into(),
        cwd: unrelated.path().into(),
        log,
    };
    assert!(
        wait_for(Duration::from_secs(3), || {
            let status = keep(
                config_dir.path(),
                runtime_dir.path(),
                unrelated.path(),
                &["status", "failure"],
            );
            let stdout = String::from_utf8_lossy(&status.stdout);
            status.status.success()
                && stdout.contains("failure\tservice\t-\tfailed\t")
                && stdout.contains("failure\tdownstream\t-\tblocked\t")
                && stdout.contains("timed out")
        }),
        "{}",
        guard.logs()
    );
    let combined = guard.logs();
    assert!(
        combined.contains("readiness failed for 'service'"),
        "{combined}"
    );
    assert!(!combined.contains("should-not-run"), "{combined}");
}

#[test]
fn command_probe_obeys_the_startup_deadline_and_kills_its_process_group() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let probe_pid = root.path().join("probe-child.pid");
    fs::write(
        config_dir.path().join("deadline.yaml"),
        format!(
            r#"
version: 1
project:
  id: deadline
  path: {}
processes:
  service:
    command: "trap 'exit 0' TERM; while :; do sleep 1; done"
    readiness:
      type: command
      target: "sleep 30 & echo $! > '{}'; wait"
      attempt_timeout: 100ms
      startup_timeout: 200ms
      interval: 2s
"#,
            root.path().display(),
            probe_pid.display()
        ),
    )
    .unwrap();
    let log = unrelated.path().join("deadline.log");
    let stdout = File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", "deadline"])
        .env("KEEP_CONFIG_DIR", config_dir.path())
        .env("KEEP_RUNTIME_DIR", runtime_dir.path())
        .current_dir(unrelated.path())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let guard = Guard {
        child,
        project: "deadline".into(),
        config: config_dir.path().into(),
        runtime: runtime_dir.path().into(),
        cwd: unrelated.path().into(),
        log,
    };
    let started = Instant::now();
    let failed_in_time = wait_for(Duration::from_millis(1500), || {
        let status = keep(
            config_dir.path(),
            runtime_dir.path(),
            unrelated.path(),
            &["status", "deadline/service"],
        );
        status.status.success() && String::from_utf8_lossy(&status.stdout).contains("\tfailed\t")
    });
    let elapsed = started.elapsed();
    let pid = wait_for(Duration::from_secs(1), || probe_pid.is_file())
        .then(|| fs::read_to_string(&probe_pid).unwrap())
        .and_then(|value| value.trim().parse::<i32>().ok());
    let descendant_stopped = pid.is_some_and(|pid| {
        wait_for(Duration::from_secs(1), || {
            signal::kill(Pid::from_raw(pid), None).is_err()
        })
    });
    if let Some(pid) = pid {
        if !descendant_stopped {
            let _ = signal::kill(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
    drop(guard);
    assert!(failed_in_time, "deadline took {elapsed:?}");
    assert!(descendant_stopped, "command probe leaked child {pid:?}");
}
