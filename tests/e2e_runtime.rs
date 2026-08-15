use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn runtime_temp_dir() -> TempDir {
    TempDir::new_in("/tmp").expect("short runtime directory")
}

struct RunningKeep {
    child: Child,
    project: String,
    config_dir: PathBuf,
    runtime_dir: PathBuf,
    working_dir: PathBuf,
    log_path: PathBuf,
}

impl RunningKeep {
    fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.child.try_wait().expect("supervisor status").is_some() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn logs(&self) -> String {
        fs::read_to_string(&self.log_path).unwrap_or_default()
    }
}

impl Drop for RunningKeep {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = keep(
                &self.config_dir,
                &self.runtime_dir,
                &self.working_dir,
                &["stop", &self.project],
            );
        }
        if !self.wait_for_exit(Duration::from_secs(2)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn keep(config_dir: &Path, runtime_dir: &Path, working_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(args)
        .env("KEEP_CONFIG_DIR", config_dir)
        .env("KEEP_RUNTIME_DIR", runtime_dir)
        .current_dir(working_dir)
        .output()
        .expect("keep should execute")
}

fn spawn_keep(
    project: &str,
    config_dir: &Path,
    runtime_dir: &Path,
    working_dir: &Path,
    log_dir: &Path,
) -> RunningKeep {
    let log_path = log_dir.join(format!("{project}.log"));
    let stdout = File::create(&log_path).expect("supervisor log file");
    let stderr = stdout.try_clone().expect("clone supervisor log file");
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["start", "--config", project])
        .env("KEEP_CONFIG_DIR", config_dir)
        .env("KEEP_RUNTIME_DIR", runtime_dir)
        .current_dir(working_dir)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("keep start should execute");

    RunningKeep {
        child,
        project: project.into(),
        config_dir: config_dir.into(),
        runtime_dir: runtime_dir.into(),
        working_dir: working_dir.into(),
        log_path,
    }
}

fn wait_for(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn write_service_config(config_dir: &Path, root: &Path, project: &str, processes: &str) {
    fs::write(
        config_dir.join(format!("{project}.yaml")),
        format!(
            r#"
version: 1
project:
  id: {project}
  path: {}
processes:
{processes}
"#,
            root.display()
        ),
    )
    .expect("configuration fixture");
}

const LONG_RUNNING_PROCESS: &str = r#"    command: |
      trap 'exit 0' TERM INT HUP
      echo online
      while :; do sleep 1; done"#;

#[test]
fn start_runs_dependencies_and_global_ls_and_stop_work_from_another_directory() {
    let config_dir = TempDir::new().expect("configuration directory");
    let runtime_dir = runtime_temp_dir();
    let project_root = TempDir::new().expect("project root");
    let unrelated = TempDir::new().expect("unrelated directory");
    let marker = project_root.path().join("order.txt");
    let processes = format!(
        r#"  prepare:
    command: "printf 'prepare\\n' >> '{}'"
    mode: task
  api:
    command: |
      printf 'api\n' >> '{}'
      trap 'exit 0' TERM INT HUP
      while :; do sleep 1; done
    depends_on:
      prepare: completed_successfully"#,
        marker.display(),
        marker.display()
    );
    write_service_config(config_dir.path(), project_root.path(), "shop", &processes);

    let mut supervisor = spawn_keep(
        "shop",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        unrelated.path(),
    );

    let listed = wait_for(Duration::from_secs(5), || {
        let output = keep(
            config_dir.path(),
            runtime_dir.path(),
            unrelated.path(),
            &["ls"],
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        output.status.success()
            && stdout.lines().any(|line| {
                let columns = line.split('\t').collect::<Vec<_>>();
                columns.first() == Some(&"shop")
                    && columns.get(1) == Some(&"api")
                    && columns.get(3) == Some(&"ready")
            })
    });
    assert!(listed, "supervisor logs:\n{}", supervisor.logs());
    assert!(
        wait_for(Duration::from_secs(2), || {
            fs::read_to_string(&marker).is_ok_and(|contents| contents == "prepare\napi\n")
        }),
        "api did not run after prepare; supervisor logs:\n{}",
        supervisor.logs()
    );
    assert_eq!(
        fs::read_to_string(&marker).expect("dependency marker"),
        "prepare\napi\n"
    );

    let stop = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "shop"],
    );
    assert!(
        stop.status.success(),
        "{}\nsupervisor logs:\n{}",
        String::from_utf8_lossy(&stop.stderr),
        supervisor.logs()
    );
    assert_eq!(String::from_utf8_lossy(&stop.stdout), "stopped shop\n");
    assert!(
        supervisor.wait_for_exit(Duration::from_secs(7)),
        "supervisor logs:\n{}",
        supervisor.logs()
    );
}

#[test]
fn stop_can_target_one_process_without_stopping_its_sibling() {
    let config_dir = TempDir::new().expect("configuration directory");
    let runtime_dir = runtime_temp_dir();
    let project_root = TempDir::new().expect("project root");
    let unrelated = TempDir::new().expect("unrelated directory");
    let processes = format!("  api:\n{LONG_RUNNING_PROCESS}\n  worker:\n{LONG_RUNNING_PROCESS}");
    write_service_config(config_dir.path(), project_root.path(), "shop", &processes);
    let supervisor = spawn_keep(
        "shop",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        unrelated.path(),
    );
    assert!(
        wait_for(Duration::from_secs(5), || {
            let output = keep(
                config_dir.path(),
                runtime_dir.path(),
                unrelated.path(),
                &["ls", "shop"],
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            output.status.success()
                && stdout.contains("shop\tapi\t")
                && stdout.contains("shop\tworker\t")
        }),
        "supervisor logs:\n{}",
        supervisor.logs()
    );
    assert!(
        wait_for(Duration::from_secs(2), || supervisor
            .logs()
            .contains("api | online")),
        "supervisor logs:\n{}",
        supervisor.logs()
    );

    let stop = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "shop/worker"],
    );
    assert!(
        stop.status.success(),
        "{}\nsupervisor logs:\n{}",
        String::from_utf8_lossy(&stop.stderr),
        supervisor.logs()
    );
    let listed = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["ls", "shop"],
    );
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("shop\tworker\t-\tstopped\t"), "{stdout}");
    assert!(stdout.contains("shop\tapi\t"), "{stdout}");
    assert!(stdout.contains("\tready\t"), "{stdout}");

    let stop_project = keep(
        config_dir.path(),
        runtime_dir.path(),
        project_root.path(),
        &["stop"],
    );
    assert!(
        stop_project.status.success(),
        "{}",
        String::from_utf8_lossy(&stop_project.stderr)
    );
}

#[test]
fn ls_discovers_processes_from_multiple_foreground_supervisors() {
    let config_dir = TempDir::new().expect("configuration directory");
    let runtime_dir = runtime_temp_dir();
    let alpha_root = TempDir::new().expect("alpha project root");
    let beta_root = TempDir::new().expect("beta project root");
    let unrelated = TempDir::new().expect("unrelated directory");
    write_service_config(
        config_dir.path(),
        alpha_root.path(),
        "alpha",
        &format!("  server:\n{LONG_RUNNING_PROCESS}"),
    );
    write_service_config(
        config_dir.path(),
        beta_root.path(),
        "beta",
        &format!("  worker:\n{LONG_RUNNING_PROCESS}"),
    );
    let mut alpha = spawn_keep(
        "alpha",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        unrelated.path(),
    );
    let mut beta = spawn_keep(
        "beta",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        unrelated.path(),
    );

    assert!(
        wait_for(Duration::from_secs(5), || {
            let output = keep(
                config_dir.path(),
                runtime_dir.path(),
                unrelated.path(),
                &["ls"],
            );
            let stdout = String::from_utf8_lossy(&output.stdout);
            output.status.success()
                && stdout.contains("alpha\tserver\t")
                && stdout.contains("beta\tworker\t")
        }),
        "alpha logs:\n{}\nbeta logs:\n{}",
        alpha.logs(),
        beta.logs()
    );
    let stop_all = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "--all"],
    );
    assert!(
        stop_all.status.success(),
        "{}",
        String::from_utf8_lossy(&stop_all.stderr)
    );
    assert!(alpha.wait_for_exit(Duration::from_secs(7)));
    assert!(beta.wait_for_exit(Duration::from_secs(7)));
}

#[test]
fn ls_removes_a_registration_whose_supervisor_is_dead() {
    let config_dir = TempDir::new().expect("configuration directory");
    let runtime_dir = runtime_temp_dir();
    let unrelated = TempDir::new().expect("unrelated directory");
    let instance = runtime_dir.path().join("dead-project");
    fs::create_dir(&instance).unwrap();
    fs::write(
        instance.join("instance.json"),
        serde_json::json!({
            "protocol_version": 1,
            "project_id": "dead-project",
            "project_name": "Dead Project",
            "project_root": "/projects/dead",
            "config_path": "/configs/dead.yaml",
            "supervisor_pid": 99999999,
            "started_at_unix_seconds": 0,
            "socket": instance.join("control.sock")
        })
        .to_string(),
    )
    .unwrap();

    let output = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["ls"],
    );
    assert!(output.status.success());
    assert!(!instance.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("removed stale registration"));
}

#[test]
fn ls_uses_the_project_lock_instead_of_an_unrelated_live_pid_for_stale_detection() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let unrelated = TempDir::new().unwrap();
    let instance = runtime_dir.path().join("reused-pid");
    fs::create_dir(&instance).unwrap();
    fs::write(
        instance.join("instance.json"),
        serde_json::json!({
            "protocol_version": 1,
            "project_id": "reused-pid",
            "project_name": "Reused PID",
            "project_root": "/projects/dead",
            "config_path": "/configs/dead.yaml",
            "supervisor_pid": std::process::id(),
            "started_at_unix_seconds": 0,
            "socket": instance.join("missing.sock")
        })
        .to_string(),
    )
    .unwrap();

    let output = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["ls"],
    );
    assert!(output.status.success());
    assert!(!instance.exists());
}

#[test]
fn ls_reports_a_locked_but_unresponsive_registration_without_deleting_it() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let unrelated = TempDir::new().unwrap();
    let instance = runtime_dir.path().join("unresponsive");
    fs::create_dir(&instance).unwrap();
    fs::write(
        instance.join("instance.json"),
        serde_json::json!({
            "protocol_version": 1,
            "project_id": "unresponsive",
            "project_name": "Unresponsive",
            "project_root": "/projects/unresponsive",
            "config_path": "/configs/unresponsive.yaml",
            "supervisor_pid": std::process::id(),
            "started_at_unix_seconds": 0,
            "socket": instance.join("missing.sock")
        })
        .to_string(),
    )
    .unwrap();
    let _lock = keep::runtime::try_project_lock(runtime_dir.path(), "unresponsive")
        .unwrap()
        .unwrap();

    let output = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["ls"],
    );
    assert!(!output.status.success());
    assert!(instance.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot connect"));
}

#[test]
fn concurrent_starts_leave_exactly_one_registered_supervisor() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    write_service_config(
        config_dir.path(),
        root.path(),
        "single",
        &format!("  api:\n{LONG_RUNNING_PROCESS}"),
    );
    let mut starts = (0..8)
        .map(|_| {
            Command::new(env!("CARGO_BIN_EXE_keep"))
                .args(["start", "--config", "single"])
                .env("KEEP_CONFIG_DIR", config_dir.path())
                .env("KEEP_RUNTIME_DIR", runtime_dir.path())
                .current_dir(unrelated.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(wait_for(Duration::from_secs(5), || {
        keep(
            config_dir.path(),
            runtime_dir.path(),
            unrelated.path(),
            &["status", "single"],
        )
        .status
        .success()
    }));
    thread::sleep(Duration::from_millis(300));
    let mut running = 0;
    for child in &mut starts {
        if child.try_wait().unwrap().is_none() {
            running += 1;
        }
    }
    let _ = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "single"],
    );
    for child in &mut starts {
        if !wait_for(Duration::from_secs(2), || {
            child.try_wait().unwrap().is_some()
        }) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    assert_eq!(running, 1);
}
