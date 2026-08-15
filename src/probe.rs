use std::collections::BTreeMap;
use std::io::BufReader;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal};
use nix::unistd::{setsid, Pid};

use crate::config::{ProbeType, ReadinessConfig};

#[derive(Debug)]
pub enum ProbeEvent {
    Ready {
        process: String,
        generation: u64,
    },
    Failed {
        process: String,
        generation: u64,
        reason: String,
    },
}

pub fn spawn_readiness_probe(
    process: String,
    generation: u64,
    config: ReadinessConfig,
    root: PathBuf,
    environment: BTreeMap<String, String>,
    cancel: Arc<AtomicBool>,
    sender: Sender<ProbeEvent>,
) {
    thread::spawn(move || {
        let interval = duration_or(&config.interval, Duration::from_secs(1));
        let attempt_timeout = duration_or(&config.attempt_timeout, Duration::from_secs(1));
        let startup_timeout = duration_or(&config.startup_timeout, Duration::from_secs(30));
        let threshold = config.success_threshold.unwrap_or(1);
        let Some(deadline) = Instant::now().checked_add(startup_timeout) else {
            let _ = sender.send(ProbeEvent::Failed {
                process,
                generation,
                reason: "startup timeout is too large for this platform".into(),
            });
            return;
        };
        let mut successes = 0;
        let mut last_error = "probe did not run".to_string();

        loop {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                let _ = sender.send(ProbeEvent::Failed {
                    process,
                    generation,
                    reason: format!("timed out after {startup_timeout:?}: {last_error}"),
                });
                return;
            }
            let remaining = deadline.saturating_duration_since(now);
            match probe_once(&config, &root, &environment, attempt_timeout.min(remaining)) {
                Ok(()) => {
                    if Instant::now() > deadline {
                        last_error = "probe exceeded the startup timeout".into();
                        continue;
                    }
                    successes += 1;
                    if successes >= threshold {
                        let _ = sender.send(ProbeEvent::Ready {
                            process,
                            generation,
                        });
                        return;
                    }
                }
                Err(error) => {
                    successes = 0;
                    last_error = error;
                }
            }

            if Instant::now() >= deadline {
                let _ = sender.send(ProbeEvent::Failed {
                    process,
                    generation,
                    reason: format!("timed out after {startup_timeout:?}: {last_error}"),
                });
                return;
            }
            sleep_cancellable(
                interval.min(deadline.saturating_duration_since(Instant::now())),
                &cancel,
            );
        }
    });
}

fn duration_or(value: &Option<String>, fallback: Duration) -> Duration {
    value
        .as_deref()
        .and_then(|value| humantime::parse_duration(value).ok())
        .unwrap_or(fallback)
}

fn sleep_cancellable(duration: Duration, cancel: &AtomicBool) {
    let Some(deadline) = Instant::now().checked_add(duration) else {
        return;
    };
    while Instant::now() < deadline && !cancel.load(Ordering::Relaxed) {
        thread::sleep(
            Duration::from_millis(25).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn probe_once(
    config: &ReadinessConfig,
    root: &Path,
    environment: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<(), String> {
    match config.kind {
        ProbeType::Tcp | ProbeType::Tcp4 | ProbeType::Tcp6 => {
            probe_tcp(config.kind, &config.target, timeout)
        }
        ProbeType::Http | ProbeType::Https => probe_http(config, root, timeout),
        ProbeType::Unix => UnixStream::connect(strip_scheme(&config.target, "unix://"))
            .map(|_| ())
            .map_err(|error| error.to_string()),
        ProbeType::File => {
            let target = strip_scheme(&config.target, "file://");
            let path = PathBuf::from(target);
            let path = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            path.is_file()
                .then_some(())
                .ok_or_else(|| format!("file does not exist: {}", path.display()))
        }
        ProbeType::Command => probe_command(&config.target, root, environment, timeout),
    }
}

fn probe_tcp(kind: ProbeType, target: &str, timeout: Duration) -> Result<(), String> {
    let target = target
        .strip_prefix("tcp://")
        .or_else(|| target.strip_prefix("tcp4://"))
        .or_else(|| target.strip_prefix("tcp6://"))
        .unwrap_or(target);
    let addresses = target
        .to_socket_addrs()
        .map_err(|error| format!("cannot resolve {target}: {error}"))?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "TCP attempt timeout is too large for this platform".to_string())?;
    let mut attempted = false;
    let mut last_error = None;
    for address in addresses.filter(|address| match kind {
        ProbeType::Tcp4 => address.is_ipv4(),
        ProbeType::Tcp6 => address.is_ipv6(),
        _ => true,
    }) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            last_error = Some(format!("connection attempt exceeded {timeout:?}"));
            break;
        }
        attempted = true;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    if !attempted {
        Err(format!("no compatible address resolved for {target}"))
    } else {
        Err(last_error.unwrap_or_else(|| format!("cannot connect to {target}")))
    }
}

fn probe_http(config: &ReadinessConfig, root: &Path, timeout: Duration) -> Result<(), String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout(timeout);
    if let Some(ca_path) = &config.tls_ca {
        let ca_path = PathBuf::from(ca_path);
        let ca_path = if ca_path.is_absolute() {
            ca_path
        } else {
            root.join(ca_path)
        };
        let file = std::fs::File::open(&ca_path)
            .map_err(|error| format!("cannot open TLS CA {}: {error}", ca_path.display()))?;
        let mut reader = BufReader::new(file);
        let certificates = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("cannot parse TLS CA {}: {error}", ca_path.display()))?;
        let mut roots = rustls::RootCertStore::empty();
        for certificate in certificates {
            roots
                .add(certificate)
                .map_err(|error| format!("invalid TLS CA {}: {error}", ca_path.display()))?;
        }
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        builder = builder.tls_config(Arc::new(tls));
    }
    let agent = builder.build();
    let method = config.method.as_deref().unwrap_or("GET");
    let mut request = agent.request(method, &config.target);
    for (name, value) in &config.headers {
        request = request.set(name, value);
    }
    let status = match request.call() {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(status, _)) => status,
        Err(ureq::Error::Transport(error)) => return Err(error.to_string()),
    };
    let accepted = config
        .expected_status
        .map_or((200..300).contains(&status), |expected| status == expected);
    accepted
        .then_some(())
        .ok_or_else(|| format!("received HTTP status {status}"))
}

fn probe_command(
    command: &str,
    root: &Path,
    environment: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<(), String> {
    let mut child_command = Command::new("sh");
    child_command
        .args(["-c", command])
        .current_dir(root)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        child_command.pre_exec(|| {
            setsid().map_err(std::io::Error::other)?;
            Ok(())
        });
    }
    let mut child = child_command.spawn().map_err(|error| error.to_string())?;
    let group = Pid::from_raw(child.id() as i32);
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "command attempt timeout is too large for this platform".to_string())?;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = signal::killpg(group, Signal::SIGKILL);
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("command exited with {status}"))
                };
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                let _ = signal::killpg(group, Signal::SIGKILL);
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    }
    let _ = signal::killpg(group, Signal::SIGKILL);
    let _ = child.wait();
    Err(format!("command probe exceeded {timeout:?}"))
}

fn strip_scheme<'a>(target: &'a str, scheme: &str) -> &'a str {
    target.strip_prefix(scheme).unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn readiness(kind: ProbeType, target: String) -> ReadinessConfig {
        ReadinessConfig {
            kind,
            target,
            interval: None,
            attempt_timeout: None,
            startup_timeout: None,
            success_threshold: None,
            method: None,
            headers: BTreeMap::new(),
            expected_status: None,
            tls_ca: None,
        }
    }

    #[test]
    fn tcp_probe_connects_to_a_real_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("TCP listener");
        let config = readiness(ProbeType::Tcp, listener.local_addr().unwrap().to_string());
        assert_eq!(
            probe_once(
                &config,
                Path::new("/"),
                &BTreeMap::new(),
                Duration::from_secs(1)
            ),
            Ok(())
        );
    }

    #[test]
    fn command_probe_uses_its_exit_status() {
        let config = readiness(ProbeType::Command, "exit 7".into());
        assert!(probe_once(
            &config,
            Path::new("/"),
            &BTreeMap::new(),
            Duration::from_secs(1)
        )
        .expect_err("failing command")
        .contains("exit status"));
    }
}
