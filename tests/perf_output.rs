use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::sys::resource::{getrusage, UsageWho};
use tempfile::TempDir;

const DEFAULT_LINES: u64 = 100_000;
const DEFAULT_MAX_SECONDS: f64 = 5.0;
const DEFAULT_MAX_RSS_MIB: u64 = 64;

#[test]
#[ignore = "run explicitly with `just perf`"]
fn output_pipeline_stays_within_its_performance_budget() {
    let lines = environment_u64("KEEP_PERF_LINES", DEFAULT_LINES);
    let max_seconds = environment_f64("KEEP_PERF_MAX_SECONDS", DEFAULT_MAX_SECONDS);
    let max_rss_mib = environment_u64("KEEP_PERF_MAX_RSS_MIB", DEFAULT_MAX_RSS_MIB);
    let config = TempDir::new().unwrap();
    let runtime = TempDir::new_in("/tmp").unwrap();
    let root = TempDir::new().unwrap();
    fs::write(
        config.path().join("performance.yaml"),
        format!(
            r#"
version: 1
project:
  id: performance
  path: {}
processes:
  burst:
    mode: task
    command: |
      awk 'BEGIN {{ for (i = 0; i < {lines}; i++) print "0123456789012345678901234567890123456789012345678901234567890123" }}'
"#,
            root.path().display()
        ),
    )
    .unwrap();

    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args([
            "start",
            "--config",
            config.path().join("performance.yaml").to_str().unwrap(),
        ])
        .env("KEEP_RUNTIME_DIR", runtime.path())
        .current_dir(root.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    let max_rss_bytes = child_max_rss_bytes();
    let max_rss_mib_actual = max_rss_bytes as f64 / 1024.0 / 1024.0;
    let lines_per_second = lines as f64 / elapsed.as_secs_f64();

    println!(
        "lines={lines} elapsed={elapsed:?} lines_per_second={lines_per_second:.0} max_rss_mib={max_rss_mib_actual:.1}"
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed <= Duration::from_secs_f64(max_seconds),
        "output benchmark took {elapsed:?}; budget is {max_seconds}s"
    );
    assert!(
        max_rss_bytes <= max_rss_mib * 1024 * 1024,
        "output benchmark used {max_rss_mib_actual:.1} MiB RSS; budget is {max_rss_mib} MiB"
    );
}

fn environment_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn environment_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn child_max_rss_bytes() -> u64 {
    let max_rss = getrusage(UsageWho::RUSAGE_CHILDREN).unwrap().max_rss() as u64;
    if cfg!(target_os = "macos") {
        max_rss
    } else {
        max_rss * 1024
    }
}
