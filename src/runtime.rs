use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nix::fcntl::{Flock, FlockArg};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("cannot prepare runtime directory {path}: {source}")]
    PrepareDirectory {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot read runtime directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot read runtime metadata {path}: {source}")]
    ReadMetadata {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid runtime metadata {path}: {source}")]
    InvalidMetadata {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("cannot encode runtime message: {0}")]
    Encode(serde_json::Error),

    #[error("cannot connect to project '{project}' at {socket}: {source}")]
    Connect {
        project: String,
        socket: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot communicate with project '{project}': {source}")]
    Communicate {
        project: String,
        source: std::io::Error,
    },

    #[error("invalid response from project '{project}': {message}")]
    InvalidResponse { project: String, message: String },

    #[error("project '{0}' is not running")]
    ProjectNotRunning(String),

    #[error("process '{project}/{process}' is not present in the running project")]
    ProcessNotRunning { project: String, process: String },

    #[error("project '{project}' rejected the request: {message}")]
    Rejected { project: String, message: String },

    #[error("cannot open project lock {path}: {source}")]
    OpenLock {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot lock project '{project}': {message}")]
    LockProject { project: String, message: String },
}

#[derive(Debug)]
pub struct ProjectLock {
    _file: Flock<File>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceMetadata {
    pub protocol_version: u32,
    pub project_id: String,
    pub project_name: String,
    pub project_root: PathBuf,
    pub config_path: PathBuf,
    pub supervisor_pid: u32,
    pub started_at_unix_seconds: u64,
    pub socket: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ControlRequest {
    Ping {
        version: u32,
    },
    Status {
        version: u32,
    },
    StartProcesses {
        version: u32,
        processes: Vec<String>,
    },
    StopProcesses {
        version: u32,
        processes: Vec<String>,
    },
    RestartProcesses {
        version: u32,
        processes: Vec<String>,
    },
    Shutdown {
        version: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub version: u32,
    pub ok: bool,
    pub message: Option<String>,
    pub project: Option<ProjectStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectStatus {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub supervisor_pid: u32,
    pub processes: Vec<ProcessStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub name: String,
    pub pid: Option<u32>,
    pub state: ProcessState,
    pub detail: Option<String>,
    pub restart_count: u32,
    #[serde(default)]
    pub started_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    Pending,
    Blocked,
    Starting,
    Running,
    Checking,
    Completed,
    Failed,
    Restarting,
    Stopping,
    Stopped,
}

impl std::fmt::Display for ProcessState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Checking => "checking",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Restarting => "restarting",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        };
        formatter.write_str(value)
    }
}

impl std::str::FromStr for ProcessState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "pending" => Self::Pending,
            "blocked" => Self::Blocked,
            "starting" => Self::Starting,
            "running" => Self::Running,
            "checking" => Self::Checking,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "restarting" => Self::Restarting,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            other => return Err(format!("unknown state '{other}'")),
        })
    }
}

pub fn runtime_directory() -> Result<PathBuf, RuntimeError> {
    let path = if let Some(override_path) = env::var_os("KEEP_RUNTIME_DIR") {
        PathBuf::from(override_path)
    } else if let Some(xdg_runtime) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(xdg_runtime).join("keep")
    } else {
        PathBuf::from("/tmp").join(format!("keep-{}", nix::unistd::geteuid().as_raw()))
    };

    fs::create_dir_all(&path).map_err(|source| RuntimeError::PrepareDirectory {
        path: path.clone(),
        source,
    })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        RuntimeError::PrepareDirectory {
            path: path.clone(),
            source,
        }
    })?;
    Ok(path)
}

pub fn instance_directory(runtime: &Path, project_id: &str) -> PathBuf {
    runtime.join(project_id)
}

pub fn try_project_lock(
    runtime: &Path,
    project_id: &str,
) -> Result<Option<ProjectLock>, RuntimeError> {
    let path = runtime.join(format!("{project_id}.lock"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|source| RuntimeError::OpenLock {
            path: path.clone(),
            source,
        })?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(file) => Ok(Some(ProjectLock { _file: file })),
        Err((_file, error)) if error == nix::errno::Errno::EWOULDBLOCK => Ok(None),
        Err((_file, error)) => Err(RuntimeError::LockProject {
            project: project_id.into(),
            message: error.to_string(),
        }),
    }
}

pub fn metadata_path(instance: &Path) -> PathBuf {
    instance.join("instance.json")
}

pub fn socket_path(instance: &Path) -> PathBuf {
    instance.join("control.sock")
}

pub fn write_metadata(path: &Path, metadata: &InstanceMetadata) -> Result<(), RuntimeError> {
    let encoded = serde_json::to_vec_pretty(metadata).map_err(RuntimeError::Encode)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, encoded).map_err(|source| RuntimeError::ReadMetadata {
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| RuntimeError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })
}

pub fn read_metadata(path: &Path) -> Result<InstanceMetadata, RuntimeError> {
    let bytes = fs::read(path).map_err(|source| RuntimeError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| RuntimeError::InvalidMetadata {
        path: path.to_path_buf(),
        source,
    })
}

pub fn list_metadata(runtime: &Path) -> Result<Vec<InstanceMetadata>, RuntimeError> {
    let entries = fs::read_dir(runtime).map_err(|source| RuntimeError::ReadDirectory {
        path: runtime.to_path_buf(),
        source,
    })?;
    let mut instances = Vec::new();
    for entry in entries.flatten() {
        let path = metadata_path(&entry.path());
        if path.is_file() {
            if let Ok(metadata) = read_metadata(&path) {
                instances.push(metadata);
            }
        }
    }
    instances.sort_by(|left, right| left.project_id.cmp(&right.project_id));
    Ok(instances)
}

pub fn find_metadata(runtime: &Path, project: &str) -> Result<InstanceMetadata, RuntimeError> {
    let path = metadata_path(&instance_directory(runtime, project));
    if !path.is_file() {
        return Err(RuntimeError::ProjectNotRunning(project.into()));
    }
    read_metadata(&path)
}

pub fn send_request(
    metadata: &InstanceMetadata,
    request: &ControlRequest,
) -> Result<ControlResponse, RuntimeError> {
    let mut stream =
        UnixStream::connect(&metadata.socket).map_err(|source| RuntimeError::Connect {
            project: metadata.project_id.clone(),
            socket: metadata.socket.clone(),
            source,
        })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|source| RuntimeError::Communicate {
            project: metadata.project_id.clone(),
            source,
        })?;
    let read_timeout = match request {
        ControlRequest::StopProcesses { .. }
        | ControlRequest::RestartProcesses { .. }
        | ControlRequest::Shutdown { .. } => None,
        _ => Some(Duration::from_secs(5)),
    };
    stream
        .set_read_timeout(read_timeout)
        .map_err(|source| RuntimeError::Communicate {
            project: metadata.project_id.clone(),
            source,
        })?;
    let mut encoded = serde_json::to_vec(request).map_err(RuntimeError::Encode)?;
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .map_err(|source| RuntimeError::Communicate {
            project: metadata.project_id.clone(),
            source,
        })?;

    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|source| RuntimeError::Communicate {
            project: metadata.project_id.clone(),
            source,
        })?;
    let response: ControlResponse =
        serde_json::from_str(&response).map_err(|source| RuntimeError::InvalidResponse {
            project: metadata.project_id.clone(),
            message: source.to_string(),
        })?;
    if response.version != PROTOCOL_VERSION {
        return Err(RuntimeError::InvalidResponse {
            project: metadata.project_id.clone(),
            message: format!("unsupported protocol version {}", response.version),
        });
    }
    if !response.ok {
        return Err(RuntimeError::Rejected {
            project: metadata.project_id.clone(),
            message: response.message.unwrap_or_else(|| "unknown error".into()),
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_requests_have_an_explicit_version_and_command() {
        let request = ControlRequest::StopProcesses {
            version: PROTOCOL_VERSION,
            processes: vec!["api".into()],
        };

        assert_eq!(
            serde_json::to_value(request).expect("request should serialize"),
            serde_json::json!({
                "command": "stop_processes",
                "version": 1,
                "processes": ["api"]
            })
        );
    }

    #[test]
    fn project_status_round_trips_without_losing_process_state() {
        let response = ControlResponse {
            version: PROTOCOL_VERSION,
            ok: true,
            message: None,
            project: Some(ProjectStatus {
                id: "shop".into(),
                name: "Shop".into(),
                root: PathBuf::from("/projects/shop"),
                supervisor_pid: 42,
                processes: vec![ProcessStatus {
                    name: "api".into(),
                    pid: Some(43),
                    state: ProcessState::Running,
                    detail: None,
                    restart_count: 0,
                    started_at_unix_seconds: None,
                }],
            }),
        };

        let encoded = serde_json::to_vec(&response).expect("response should serialize");
        let decoded: ControlResponse =
            serde_json::from_slice(&encoded).expect("response should deserialize");
        assert_eq!(
            decoded.project.expect("project status").processes[0].state,
            ProcessState::Running
        );
    }

    #[test]
    fn project_lock_is_exclusive_and_released_on_drop() {
        let runtime = tempfile::TempDir::new().expect("runtime directory");
        let first = try_project_lock(runtime.path(), "shop")
            .expect("first lock attempt")
            .expect("first lock");
        assert!(
            try_project_lock(runtime.path(), "shop")
                .expect("second lock attempt")
                .is_none(),
            "a second supervisor acquired the same project lock"
        );
        drop(first);
        assert!(
            try_project_lock(runtime.path(), "shop")
                .expect("third lock attempt")
                .is_some(),
            "dropping the supervisor did not release its lock"
        );
    }
}
