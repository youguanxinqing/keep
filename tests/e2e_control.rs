use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

struct SupervisorGuard {
    child: Child,
    project: String,
    config_dir: PathBuf,
    runtime_dir: PathBuf,
    working_dir: PathBuf,
    log: PathBuf,
}

impl SupervisorGuard {
    fn logs(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        wait_for(timeout, || self.child.try_wait().ok().flatten().is_some())
    }
}

impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = keep(
                &self.config_dir,
                &self.runtime_dir,
                &self.working_dir,
                &["stop", &self.project],
            );
        }
        if !self.wait(Duration::from_secs(2)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn runtime_temp_dir() -> TempDir {
    TempDir::new_in("/tmp").expect("short runtime directory")
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

fn spawn_supervisor(
    project: &str,
    config: &Path,
    runtime: &Path,
    cwd: &Path,
    selected: &[&str],
) -> SupervisorGuard {
    let log = cwd.join(format!("{project}-control.log"));
    let stdout = File::create(&log).expect("log file");
    let stderr = stdout.try_clone().expect("cloned log file");
    let mut args = vec!["start", "--config", project];
    args.extend_from_slice(selected);
    let child = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(args)
        .env("KEEP_CONFIG_DIR", config)
        .env("KEEP_RUNTIME_DIR", runtime)
        .current_dir(cwd)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("keep start");
    SupervisorGuard {
        child,
        project: project.into(),
        config_dir: config.into(),
        runtime_dir: runtime.into(),
        working_dir: cwd.into(),
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

fn long_command(marker: &Path) -> String {
    format!(
        r#"      echo $$ > '{}'
      trap 'exit 0' TERM INT HUP
      while :; do sleep 1; done"#,
        marker.display()
    )
}

#[test]
fn status_restart_and_quit_control_a_project_globally() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let project_root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let pid_file = project_root.path().join("api.pid");
    fs::write(
        config_dir.path().join("shop.yaml"),
        format!(
            r#"
version: 1
project:
  id: shop
  path: {}
processes:
  api:
    command: |
{}
"#,
            project_root.path().display(),
            long_command(&pid_file)
        ),
    )
    .unwrap();
    let mut supervisor = spawn_supervisor(
        "shop",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &[],
    );
    assert!(wait_for(Duration::from_secs(5), || pid_file.is_file()));
    let first_pid = fs::read_to_string(&pid_file).unwrap();

    let status = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["status", "shop/api"],
    );
    assert!(status.status.success(), "{}", supervisor.logs());
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("STATUS\tRESTARTS\tDETAIL"));
    assert!(stdout.contains("shop\tapi\t"));
    assert!(stdout.contains("\tready\t0\t"));

    let status_all = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["status"],
    );
    assert!(status_all.status.success());
    assert!(String::from_utf8_lossy(&status_all.stdout).contains("shop\tapi\t"));

    let restart_current = keep(
        config_dir.path(),
        runtime_dir.path(),
        project_root.path(),
        &["restart"],
    );
    assert!(restart_current.status.success(), "{}", supervisor.logs());
    assert!(wait_for(Duration::from_secs(5), || {
        fs::read_to_string(&pid_file).is_ok_and(|current| current != first_pid)
    }));
    let current_pid = fs::read_to_string(&pid_file).unwrap();

    let restart = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["restart", "shop/api"],
    );
    assert!(restart.status.success(), "{}", supervisor.logs());
    assert!(wait_for(Duration::from_secs(5), || {
        fs::read_to_string(&pid_file).is_ok_and(|current| current != current_pid)
    }));
    let second_pid = fs::read_to_string(&pid_file).unwrap();
    let restart_project = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["restart", "shop"],
    );
    assert!(restart_project.status.success(), "{}", supervisor.logs());
    assert!(wait_for(Duration::from_secs(5), || {
        fs::read_to_string(&pid_file).is_ok_and(|current| current != second_pid)
    }));

    let quit = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["quit", "shop"],
    );
    assert!(quit.status.success(), "{}", supervisor.logs());
    assert_eq!(String::from_utf8_lossy(&quit.stdout), "quit shop\n");
    assert!(
        supervisor.wait(Duration::from_secs(7)),
        "{}",
        supervisor.logs()
    );
}

#[test]
fn start_selected_processes_includes_dependencies_and_can_enable_more_later() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let project_root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let api_pid = project_root.path().join("api.pid");
    let worker_pid = project_root.path().join("worker.pid");
    fs::write(
        config_dir.path().join("shop.yaml"),
        format!(
            r#"
version: 1
project:
  id: shop
  path: {}
processes:
  prepare:
    command: "touch prepared"
    mode: task
  api:
    command: |
{}
    depends_on:
      prepare: completed_successfully
  worker:
    command: |
{}
    depends_on:
      prepare: completed_successfully
"#,
            project_root.path().display(),
            long_command(&api_pid),
            long_command(&worker_pid)
        ),
    )
    .unwrap();
    let supervisor = spawn_supervisor(
        "shop",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["api"],
    );
    assert!(wait_for(Duration::from_secs(5), || api_pid.is_file()));
    let initial = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["ls", "shop"],
    );
    let stdout = String::from_utf8_lossy(&initial.stdout);
    assert!(stdout.contains("shop\tprepare\t-\tcompleted"), "{stdout}");
    assert!(stdout.contains("shop\tworker\t-\tstopped"), "{stdout}");

    let start_worker = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["start", "--config", "shop", "worker"],
    );
    assert!(start_worker.status.success(), "{}", supervisor.logs());
    assert!(wait_for(Duration::from_secs(5), || worker_pid.is_file()));

    let stop_both = keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "shop/api", "shop/worker"],
    );
    assert!(stop_both.status.success(), "{}", supervisor.logs());
}

#[test]
fn start_uses_the_discovered_git_root_when_configuration_has_no_path() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let project = TempDir::new().unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(project.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["remote", "add", "origin", "git@github.com:acme/rooted.git"])
        .current_dir(project.path())
        .status()
        .unwrap()
        .success());
    fs::write(
        config_dir.path().join("rooted.yaml"),
        r#"
version: 1
project:
  id: rooted
  git: [https://github.com/acme/rooted]
processes:
  pwd:
    mode: task
    command: "pwd > actual-root"
"#,
    )
    .unwrap();

    let output = keep(
        config_dir.path(),
        runtime_dir.path(),
        project.path(),
        &["start"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(project.path().join("actual-root"))
            .unwrap()
            .trim(),
        project.path().canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn start_prefers_repository_local_config_over_a_matching_global_config() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let workspace = TempDir::new().unwrap();
    let repository = workspace.path().join("shop");
    let nested = repository.join("services/api");
    fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    fs::write(
        repository.join("keep.yaml"),
        r#"
version: 1
project:
  name: local
processes:
  selected:
    mode: task
    command: touch local-started
"#,
    )
    .unwrap();
    fs::write(
        config_dir.path().join("global.yaml"),
        format!(
            r#"
version: 1
project:
  name: global
  path: {}
processes:
  selected:
    mode: task
    command: touch global-started
"#,
            repository.display()
        ),
    )
    .unwrap();

    let output = keep(config_dir.path(), runtime_dir.path(), &nested, &["start"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(repository.join("local-started").is_file());
    assert!(!repository.join("global-started").exists());
}

#[test]
fn restarting_a_dependency_preserves_reverse_dependency_shutdown_order() {
    let config_dir = TempDir::new().unwrap();
    let runtime_dir = runtime_temp_dir();
    let root = TempDir::new().unwrap();
    let unrelated = TempDir::new().unwrap();
    let order = root.path().join("stop-order");
    fs::write(
        config_dir.path().join("ordered.yaml"),
        format!(
            r#"
version: 1
project:
  id: ordered
  path: {}
processes:
  database:
    command: "trap 'echo database >> {}; exit 0' TERM; while :; do sleep 1; done"
  api:
    command: "trap 'echo api >> {}; exit 0' TERM; while :; do sleep 1; done"
    depends_on:
      database: ready
"#,
            root.path().display(),
            order.display(),
            order.display()
        ),
    )
    .unwrap();
    let mut supervisor = spawn_supervisor(
        "ordered",
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &[],
    );
    assert!(wait_for(Duration::from_secs(5), || {
        keep(
            config_dir.path(),
            runtime_dir.path(),
            unrelated.path(),
            &["status", "ordered"],
        )
        .status
        .success()
    }));
    assert!(keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["restart", "ordered/database"],
    )
    .status
    .success());
    assert!(wait_for(Duration::from_secs(3), || {
        fs::read_to_string(&order).is_ok_and(|contents| contents == "database\n")
    }));
    assert!(keep(
        config_dir.path(),
        runtime_dir.path(),
        unrelated.path(),
        &["stop", "ordered"],
    )
    .status
    .success());
    assert!(supervisor.wait(Duration::from_secs(7)));
    assert_eq!(
        fs::read_to_string(order).unwrap(),
        "database\napi\ndatabase\n"
    );
}
