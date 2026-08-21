use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

struct ProcfileGuard {
    child: Child,
    config: PathBuf,
    runtime: PathBuf,
    cwd: PathBuf,
    project: String,
}

impl Drop for ProcfileGuard {
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

fn keep(config: &Path, runtime: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(args)
        .env("KEEP_CONFIG_DIR", config)
        .env("KEEP_RUNTIME_DIR", runtime)
        .current_dir(cwd)
        .output()
        .unwrap()
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

#[test]
fn procfile_convert_emits_native_version_one_yaml() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let project = TempDir::new().unwrap();
    let procfile = project.path().join("Procfile.dev");
    fs::write(&procfile, "web: run-web\nworker: run-worker\n").unwrap();

    let output = keep(
        config.path(),
        runtime.path(),
        project.path(),
        &[
            "procfile",
            "convert",
            "--file",
            procfile.to_str().unwrap(),
            "--project",
            "legacy",
        ],
    );
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("version: 1"), "{stdout}");
    assert!(stdout.contains("id: legacy"), "{stdout}");
    assert!(stdout.contains("web:"), "{stdout}");
    assert!(stdout.contains("command: run-web"), "{stdout}");
}

#[test]
fn procfile_start_registers_processes_for_global_commands() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let project = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let procfile = project.path().join("Procfile");
    fs::write(project.path().join(".env"), "LEGACY_ENV=loaded\n").unwrap();
    fs::write(
        &procfile,
        "web: echo \"$LEGACY_ENV\" > legacy-env; trap 'exit 0' TERM; while :; do sleep 1; done\nworker: trap 'exit 0' TERM; while :; do sleep 1; done\n",
    )
    .unwrap();
    let log = unrelated.path().join("procfile.log");
    let stdout = File::create(&log).unwrap();
    let stderr = stdout.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args([
            "procfile",
            "start",
            "--file",
            procfile.to_str().unwrap(),
            "--project",
            "legacy",
        ])
        .env("KEEP_CONFIG_DIR", config.path())
        .env("KEEP_RUNTIME_DIR", runtime.path())
        .current_dir(unrelated.path())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    let mut guard = ProcfileGuard {
        child,
        config: config.path().into(),
        runtime: runtime.path().into(),
        cwd: unrelated.path().into(),
        project: "legacy".into(),
    };

    assert!(wait_for(Duration::from_secs(5), || {
        let output = keep(
            config.path(),
            runtime.path(),
            unrelated.path(),
            &["ls", "legacy"],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let has_process = |name| {
            stdout.lines().any(|line| {
                let mut fields = line.split_whitespace();
                fields.next() == Some("legacy") && fields.next() == Some(name)
            })
        };
        output.status.success() && has_process("web") && has_process("worker")
    }));
    let legacy_env = project.path().join("legacy-env");
    assert!(
        wait_for(Duration::from_secs(5), || {
            fs::read_to_string(&legacy_env).is_ok_and(|value| value.trim() == "loaded")
        }),
        "{:?}",
        fs::read_to_string(&legacy_env)
    );
    let restart = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["restart", "legacy/worker"],
    );
    assert!(
        restart.status.success(),
        "{}",
        String::from_utf8_lossy(&restart.stderr)
    );
    let stop = keep(
        config.path(),
        runtime.path(),
        unrelated.path(),
        &["stop", "legacy"],
    );
    assert!(
        stop.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&stop.stderr),
        fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(wait_for(Duration::from_secs(7), || guard
        .child
        .try_wait()
        .ok()
        .flatten()
        .is_some()));
}

#[test]
fn doctor_checks_configuration_runtime_shell_and_git() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    fs::write(
        config.path().join("valid.yaml"),
        r#"
version: 1
project:
  id: valid
processes:
  dev:
    command: dev
"#,
    )
    .unwrap();

    let output = keep(config.path(), runtime.path(), config.path(), &["doctor"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ok   configuration:"), "{stdout}");
    assert!(stdout.contains("ok   runtime:"), "{stdout}");
    assert!(stdout.contains("ok   shell:"), "{stdout}");
    assert!(stdout.contains("git:"), "{stdout}");
}

#[test]
fn doctor_fails_when_configuration_is_invalid() {
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    fs::write(config.path().join("broken.yaml"), "not: valid: yaml").unwrap();

    let output = keep(config.path(), runtime.path(), config.path(), &["doctor"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("fail configuration:"));
}
