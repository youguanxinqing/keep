use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::IsTerminal;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::pty::{openpty, Winsize};
use nix::sys::signal::{self, Signal};
use nix::unistd::{setsid, Pid};
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use thiserror::Error;

use crate::config::{DependencyCondition, LoadedConfig, ProcessConfig, ProcessMode, RestartPolicy};
use crate::environment::{expand_environment, load_environment, EnvironmentError};
use crate::probe::{spawn_readiness_probe, ProbeEvent};
use crate::runtime::{
    self, instance_directory, metadata_path, socket_path, write_metadata, ControlRequest,
    ControlResponse, InstanceMetadata, ProcessState, ProcessStatus, ProjectLock, ProjectStatus,
    RuntimeError, PROTOCOL_VERSION,
};

const LOOP_INTERVAL: Duration = Duration::from_millis(25);
const CONTROL_IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_CONTROL_REQUEST_BYTES: usize = 16 * 1024;
const OUTPUT_QUEUE_CAPACITY: usize = 128;
const PROCESS_GROUP_EXIT_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Environment(#[from] EnvironmentError),

    #[error("project '{project}' is already running with supervisor pid {pid}")]
    AlreadyRunning { project: String, pid: u32 },

    #[error("project '{project}' is starting or its supervisor is unresponsive")]
    ProjectUnresponsive { project: String },

    #[error("project '{0}' has no project.path; an explicit root is required for execution")]
    ProjectRootMissing(String),

    #[error("cannot prepare instance directory {path}: {source}")]
    PrepareInstance {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot bind control socket {path}: {source}")]
    BindSocket {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("runtime I/O failed: {0}")]
    Io(std::io::Error),

    #[error("cannot configure signal handling: {0}")]
    SignalHandler(std::io::Error),

    #[error("cannot start process '{process}': {source}")]
    StartProcess {
        process: String,
        source: std::io::Error,
    },

    #[error("process '{process}' exited unexpectedly with {status}")]
    UnexpectedExit { process: String, status: String },

    #[error("task '{process}' failed with {status}")]
    TaskFailed { process: String, status: String },

    #[error("process '{0}' does not exist")]
    ProcessNotFound(String),
}

struct Registration {
    instance_directory: PathBuf,
    _lock: ProjectLock,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.instance_directory);
    }
}

struct ManagedProcess {
    name: String,
    config: ProcessConfig,
    environment: BTreeMap<String, String>,
    enabled: bool,
    child: Option<Child>,
    pid: Option<u32>,
    state: ProcessState,
    detail: Option<String>,
    restart_count: u32,
    next_restart: Option<Instant>,
    generation: u64,
    probe_cancel: Option<Arc<AtomicBool>>,
}

struct OutputLine {
    prefix: Arc<[u8]>,
    line: Vec<u8>,
    stderr: bool,
}

impl ManagedProcess {
    fn status(&self) -> ProcessStatus {
        ProcessStatus {
            name: self.name.clone(),
            pid: self.pid,
            state: self.state,
            detail: self.detail.clone(),
            restart_count: self.restart_count,
        }
    }

    fn cancel_probe(&mut self) {
        if let Some(cancel) = self.probe_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub struct Supervisor {
    config: LoadedConfig,
    root: PathBuf,
    listener: UnixListener,
    _registration: Registration,
    processes: Vec<ManagedProcess>,
    started_order: Vec<String>,
    stop_requested: Arc<AtomicBool>,
    probe_sender: Sender<ProbeEvent>,
    probe_receiver: Receiver<ProbeEvent>,
    output_sender: Option<SyncSender<OutputLine>>,
    output_writer: Option<JoinHandle<()>>,
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown_all();
        self.finish_output();
    }
}

impl Supervisor {
    pub fn new(config: LoadedConfig, selected: &[String]) -> Result<Self, SupervisorError> {
        let root = project_root(&config)?;
        let enabled = selected_process_closure(&config, selected)?;
        let mut processes = Vec::with_capacity(config.processes.len());
        for (name, process) in &config.processes {
            let is_enabled = enabled.contains(name);
            let environment =
                load_environment(&root, &config.env_files, &process.env_files, &process.env)?;
            processes.push(ManagedProcess {
                name: name.clone(),
                config: process.clone(),
                environment,
                enabled: is_enabled,
                child: None,
                pid: None,
                state: if is_enabled {
                    ProcessState::Pending
                } else {
                    ProcessState::Stopped
                },
                detail: (!is_enabled).then(|| "not selected".into()),
                restart_count: 0,
                next_restart: None,
                generation: 0,
                probe_cancel: None,
            });
        }

        let (listener, registration) = register_instance(&config, &root)?;
        listener
            .set_nonblocking(true)
            .map_err(|source| SupervisorError::BindSocket {
                path: runtime::socket_path(&registration.instance_directory),
                source,
            })?;
        let stop_requested = Arc::new(AtomicBool::new(false));
        for caught_signal in [SIGINT, SIGTERM, SIGHUP] {
            signal_hook::flag::register(caught_signal, Arc::clone(&stop_requested))
                .map_err(SupervisorError::SignalHandler)?;
        }
        let (probe_sender, probe_receiver) = mpsc::channel();
        let (output_sender, output_receiver) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
        let output_writer = thread::spawn(move || write_output(output_receiver));

        Ok(Self {
            config,
            root,
            listener,
            _registration: registration,
            processes,
            started_order: Vec::new(),
            stop_requested,
            probe_sender,
            probe_receiver,
            output_sender: Some(output_sender),
            output_writer: Some(output_writer),
        })
    }

    pub fn run(mut self) -> Result<(), SupervisorError> {
        let result = self.run_loop();
        self.shutdown_all();
        self.finish_output();
        result
    }

    fn run_loop(&mut self) -> Result<(), SupervisorError> {
        system_log(format_args!(
            "starting project '{}' from {}",
            self.config.project.id,
            self.root.display()
        ));
        self.start_runnable()?;

        loop {
            if self.stop_requested.load(Ordering::Relaxed) {
                self.shutdown_all();
                system_log(format_args!("stopped project '{}'", self.config.project.id));
                return Ok(());
            }

            if let Some(error) = self.process_probe_events()? {
                self.shutdown_all();
                return Err(error);
            }
            if let Some(error) = self.observe_children()? {
                self.shutdown_all();
                return Err(error);
            }
            self.activate_due_restarts();
            self.start_runnable()?;

            if self.all_enabled_processes_finished() {
                return Ok(());
            }

            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => {
                        if self.handle_connection(stream)? {
                            return Ok(());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(SupervisorError::Io(error)),
                }
            }
            thread::sleep(LOOP_INTERVAL);
        }
    }

    fn start_runnable(&mut self) -> Result<(), SupervisorError> {
        loop {
            self.update_blocked_details();
            let runnable = self
                .processes
                .iter()
                .enumerate()
                .filter(|(_, process)| {
                    process.enabled
                        && matches!(process.state, ProcessState::Pending | ProcessState::Blocked)
                })
                .filter(|(_, process)| self.dependencies_satisfied(process))
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if runnable.is_empty() {
                break;
            }
            for index in runnable {
                self.start_process(index)?;
            }
        }
        Ok(())
    }

    fn update_blocked_details(&mut self) {
        let snapshot = self
            .processes
            .iter()
            .map(|process| (process.name.clone(), process.state))
            .collect::<BTreeMap<_, _>>();
        for process in &mut self.processes {
            if !process.enabled
                || !matches!(process.state, ProcessState::Pending | ProcessState::Blocked)
            {
                continue;
            }
            let waiting = process
                .config
                .depends_on
                .iter()
                .filter(|(name, condition)| {
                    let state = snapshot[*name];
                    !condition_satisfied(state, **condition)
                })
                .map(|(name, condition)| format!("{name}:{}", condition_label(*condition)))
                .collect::<Vec<_>>();
            if waiting.is_empty() {
                process.state = ProcessState::Pending;
                process.detail = None;
            } else {
                process.state = ProcessState::Blocked;
                process.detail = Some(format!("waiting for {}", waiting.join(", ")));
            }
        }
    }

    fn dependencies_satisfied(&self, process: &ManagedProcess) -> bool {
        process
            .config
            .depends_on
            .iter()
            .all(|(dependency, condition)| {
                let target = self
                    .processes
                    .iter()
                    .find(|candidate| candidate.name == *dependency)
                    .expect("validated dependency");
                condition_satisfied(target.state, *condition)
            })
    }

    fn start_process(&mut self, index: usize) -> Result<(), SupervisorError> {
        let output_sender = self
            .output_sender
            .as_ref()
            .expect("output remains active while the supervisor runs")
            .clone();
        let process = &mut self.processes[index];
        process.state = ProcessState::Starting;
        process.detail = Some("spawning command".into());
        process.generation += 1;
        let working_directory = process
            .config
            .working_directory
            .as_deref()
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    self.root.join(path)
                }
            })
            .unwrap_or_else(|| self.root.clone());

        let mut command = Command::new("sh");
        let (stdout, stdout_reader) =
            terminal_stream().map_err(|source| SupervisorError::StartProcess {
                process: process.name.clone(),
                source,
            })?;
        let (stderr, stderr_reader) =
            terminal_stream().map_err(|source| SupervisorError::StartProcess {
                process: process.name.clone(),
                source,
            })?;
        command
            .args(["-c", &process.config.command])
            .current_dir(working_directory)
            .envs(&process.environment)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        unsafe {
            command.pre_exec(|| {
                setsid().map_err(std::io::Error::other)?;
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|source| SupervisorError::StartProcess {
                process: process.name.clone(),
                source,
            })?;
        let pid = child.id();
        forward_output(
            process.name.clone(),
            stdout_reader,
            false,
            output_sender.clone(),
        );
        forward_output(process.name.clone(), stderr_reader, true, output_sender);
        process.pid = Some(pid);
        process.child = Some(child);
        process.next_restart = None;
        if !self.started_order.contains(&process.name) {
            self.started_order.push(process.name.clone());
        }
        system_log(format_args!("started '{}' with pid {pid}", process.name));

        if process.config.mode == ProcessMode::Task {
            process.state = ProcessState::Running;
            process.detail = Some("task is running".into());
        } else if let Some(mut readiness) = process.config.readiness.clone() {
            readiness.target = expand_environment(&readiness.target, &process.environment);
            if let Some(tls_ca) = &mut readiness.tls_ca {
                *tls_ca = expand_environment(tls_ca, &process.environment);
            }
            for value in readiness.headers.values_mut() {
                *value = expand_environment(value, &process.environment);
            }
            process.state = ProcessState::Checking;
            process.detail = Some(format!(
                "waiting for {} {}",
                probe_label(readiness.kind),
                readiness.target
            ));
            let cancel = Arc::new(AtomicBool::new(false));
            process.probe_cancel = Some(Arc::clone(&cancel));
            spawn_readiness_probe(
                process.name.clone(),
                process.generation,
                readiness,
                self.root.clone(),
                process.environment.clone(),
                cancel,
                self.probe_sender.clone(),
            );
        } else {
            process.state = ProcessState::Ready;
            process.detail = None;
        }
        Ok(())
    }

    fn process_probe_events(&mut self) -> Result<Option<SupervisorError>, SupervisorError> {
        while let Ok(event) = self.probe_receiver.try_recv() {
            match event {
                ProbeEvent::Ready {
                    process,
                    generation,
                } => {
                    let managed = self.process_mut(&process)?;
                    if managed.generation == generation && managed.state == ProcessState::Checking {
                        managed.probe_cancel = None;
                        managed.state = ProcessState::Ready;
                        managed.detail = None;
                        system_log(format_args!("readiness passed for '{}'", managed.name));
                    }
                }
                ProbeEvent::Failed {
                    process,
                    generation,
                    reason,
                } => {
                    let index = self.process_index(&process)?;
                    if self.processes[index].generation != generation
                        || self.processes[index].state != ProcessState::Checking
                    {
                        continue;
                    }
                    let (signal, timeout) = self.stop_settings(index);
                    stop_managed_process(&mut self.processes[index], signal, timeout);
                    if self.schedule_restart(index, true, format!("readiness failed: {reason}")) {
                        continue;
                    }
                    self.processes[index].state = ProcessState::Failed;
                    self.processes[index].detail = Some(reason.clone());
                    system_error(format_args!("readiness failed for '{process}': {reason}"));
                }
            }
        }
        Ok(None)
    }

    fn observe_children(&mut self) -> Result<Option<SupervisorError>, SupervisorError> {
        let mut exits = Vec::new();
        for (index, process) in self.processes.iter_mut().enumerate() {
            if !matches!(
                process.state,
                ProcessState::Running | ProcessState::Checking | ProcessState::Ready
            ) {
                continue;
            }
            let Some(status) = process
                .child
                .as_mut()
                .expect("active process has a child")
                .try_wait()
                .map_err(|source| SupervisorError::StartProcess {
                    process: process.name.clone(),
                    source,
                })?
            else {
                continue;
            };
            process.cancel_probe();
            if let Some(pid) = process.pid {
                kill_process_group(Pid::from_raw(pid as i32));
            }
            process.pid = None;
            process.child = None;
            exits.push((index, status));
        }

        for (index, status) in exits {
            let is_task = self.processes[index].config.mode == ProcessMode::Task;
            if is_task && status.success() {
                self.processes[index].state = ProcessState::Completed;
                self.processes[index].detail = Some("exited with status 0".into());
                system_log(format_args!(
                    "task '{}' completed successfully",
                    self.processes[index].name
                ));
                continue;
            }

            let failed = !status.success();
            let detail = describe_status(status);
            if self.schedule_restart(index, failed, detail.clone()) {
                continue;
            }
            self.processes[index].state = ProcessState::Failed;
            self.processes[index].detail = Some(detail.clone());
            let process = self.processes[index].name.clone();
            return Ok(Some(if is_task {
                SupervisorError::TaskFailed {
                    process,
                    status: detail,
                }
            } else {
                SupervisorError::UnexpectedExit {
                    process,
                    status: detail,
                }
            }));
        }
        Ok(None)
    }

    fn schedule_restart(&mut self, index: usize, failed: bool, detail: String) -> bool {
        let restart = self.processes[index].config.restart.as_ref().or(self
            .config
            .defaults
            .restart
            .as_ref());
        let policy = restart.map_or(RestartPolicy::Never, |restart| restart.policy);
        let should_restart = match policy {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => failed,
            RestartPolicy::Always => true,
        };
        if !should_restart {
            return false;
        }
        if let Some(max) = restart.and_then(|restart| restart.max_attempts) {
            if self.processes[index].restart_count >= max {
                self.processes[index].detail =
                    Some(format!("restart limit {max} reached after {detail}"));
                return false;
            }
        }
        let backoff = restart
            .and_then(|restart| restart.backoff.as_deref())
            .and_then(|value| humantime::parse_duration(value).ok())
            .unwrap_or(Duration::from_secs(1));
        let process = &mut self.processes[index];
        process.restart_count += 1;
        process.state = ProcessState::Restarting;
        process.detail = Some(format!("{detail}; retrying in {backoff:?}"));
        process.next_restart = Instant::now().checked_add(backoff);
        system_log(format_args!(
            "scheduling restart {} for '{}'",
            process.restart_count, process.name
        ));
        true
    }

    fn activate_due_restarts(&mut self) {
        let now = Instant::now();
        for process in &mut self.processes {
            if process.state == ProcessState::Restarting
                && process.next_restart.is_some_and(|deadline| deadline <= now)
            {
                process.state = ProcessState::Pending;
                process.detail = None;
                process.next_restart = None;
            }
        }
    }

    fn handle_connection(&mut self, mut stream: UnixStream) -> Result<bool, SupervisorError> {
        if let Err(error) = stream.set_read_timeout(Some(CONTROL_IO_TIMEOUT)) {
            system_error(format_args!("rejected control connection: {error}"));
            return Ok(false);
        }
        if let Err(error) = stream.set_write_timeout(Some(CONTROL_IO_TIMEOUT)) {
            system_error(format_args!("rejected control connection: {error}"));
            return Ok(false);
        }
        let mut line = String::new();
        let cloned = match stream.try_clone() {
            Ok(cloned) => cloned,
            Err(error) => {
                system_error(format_args!("rejected control connection: {error}"));
                return Ok(false);
            }
        };
        let read = match BufReader::new(cloned)
            .take((MAX_CONTROL_REQUEST_BYTES + 1) as u64)
            .read_line(&mut line)
        {
            Ok(read) => read,
            Err(error) => {
                system_error(format_args!("rejected control connection: {error}"));
                return Ok(false);
            }
        };
        if read == 0 {
            return Ok(false);
        }
        let request = (line.len() <= MAX_CONTROL_REQUEST_BYTES)
            .then(|| serde_json::from_str::<ControlRequest>(&line));
        let (response, should_exit) = match request {
            None => (
                self.error_response(&format!(
                    "control request exceeds {MAX_CONTROL_REQUEST_BYTES} bytes"
                )),
                false,
            ),
            Some(Ok(request)) if request_version(&request) != PROTOCOL_VERSION => {
                (self.error_response("unsupported protocol version"), false)
            }
            Some(Ok(ControlRequest::Ping { .. })) | Some(Ok(ControlRequest::Status { .. })) => {
                (self.status_response(), false)
            }
            Some(Ok(ControlRequest::StartProcesses { processes, .. })) => {
                match self
                    .enable_processes(&processes)
                    .and_then(|()| self.start_runnable())
                {
                    Ok(()) => (self.status_response(), false),
                    Err(SupervisorError::ProcessNotFound(name)) => (
                        self.error_response(&format!("process '{name}' does not exist")),
                        false,
                    ),
                    Err(error) => return Err(error),
                }
            }
            Some(Ok(ControlRequest::StopProcesses { processes, .. })) => {
                match self.stop_processes(&processes) {
                    Ok(()) => (self.status_response(), false),
                    Err(SupervisorError::ProcessNotFound(name)) => (
                        self.error_response(&format!("process '{name}' does not exist")),
                        false,
                    ),
                    Err(error) => return Err(error),
                }
            }
            Some(Ok(ControlRequest::RestartProcesses { processes, .. })) => {
                match self.restart_processes(&processes) {
                    Ok(()) => (self.status_response(), false),
                    Err(SupervisorError::ProcessNotFound(name)) => (
                        self.error_response(&format!("process '{name}' does not exist")),
                        false,
                    ),
                    Err(error) => return Err(error),
                }
            }
            Some(Ok(ControlRequest::Shutdown { .. })) => {
                self.shutdown_all();
                (self.status_response(), true)
            }
            Some(Err(error)) => (
                self.error_response(&format!("invalid control request: {error}")),
                false,
            ),
        };
        let mut encoded = serde_json::to_vec(&response)
            .map_err(RuntimeError::Encode)
            .map_err(SupervisorError::Runtime)?;
        encoded.push(b'\n');
        if let Err(error) = stream.write_all(&encoded) {
            system_error(format_args!(
                "control client disconnected before response: {error}"
            ));
            return Ok(false);
        }
        Ok(should_exit)
    }

    fn status_response(&self) -> ControlResponse {
        ControlResponse {
            version: PROTOCOL_VERSION,
            ok: true,
            message: None,
            project: Some(ProjectStatus {
                id: self.config.project.id.clone(),
                name: self.config.project.display_name().into(),
                root: self.root.clone(),
                supervisor_pid: std::process::id(),
                processes: self.processes.iter().map(ManagedProcess::status).collect(),
            }),
        }
    }

    fn error_response(&self, message: &str) -> ControlResponse {
        ControlResponse {
            version: PROTOCOL_VERSION,
            ok: false,
            message: Some(message.into()),
            project: None,
        }
    }

    fn enable_processes(&mut self, names: &[String]) -> Result<(), SupervisorError> {
        let closure = process_name_closure(&self.config, names)?;
        for process in &mut self.processes {
            if closure.contains(&process.name) {
                process.enabled = true;
                if matches!(
                    process.state,
                    ProcessState::Stopped | ProcessState::Failed | ProcessState::Completed
                ) {
                    process.state = ProcessState::Pending;
                    process.detail = None;
                }
            }
        }
        Ok(())
    }

    fn stop_processes(&mut self, names: &[String]) -> Result<(), SupervisorError> {
        self.validate_process_names(names)?;
        let targets = names.iter().cloned().collect::<BTreeSet<_>>();
        let order = self.started_order.iter().rev().cloned().collect::<Vec<_>>();
        for name in order {
            if targets.contains(&name) {
                let index = self.process_index(&name)?;
                let (stop_signal, timeout) = self.stop_settings(index);
                self.processes[index].enabled = false;
                stop_managed_process(&mut self.processes[index], stop_signal, timeout);
            }
        }
        for name in names {
            let index = self.process_index(name)?;
            self.processes[index].enabled = false;
            if matches!(
                self.processes[index].state,
                ProcessState::Pending | ProcessState::Blocked | ProcessState::Restarting
            ) {
                self.processes[index].state = ProcessState::Stopped;
                self.processes[index].detail = Some("stopped by user".into());
            }
        }
        Ok(())
    }

    fn restart_processes(&mut self, names: &[String]) -> Result<(), SupervisorError> {
        let targets = if names.is_empty() {
            self.processes
                .iter()
                .filter(|process| process.enabled)
                .map(|process| process.name.clone())
                .collect::<Vec<_>>()
        } else {
            self.validate_process_names(names)?;
            names.to_vec()
        };
        let target_set = targets.iter().cloned().collect::<BTreeSet<_>>();
        let order = self.started_order.iter().rev().cloned().collect::<Vec<_>>();
        for name in order {
            if target_set.contains(&name) {
                let index = self.process_index(&name)?;
                let (stop_signal, timeout) = self.stop_settings(index);
                stop_managed_process(&mut self.processes[index], stop_signal, timeout);
            }
        }
        for name in targets {
            let index = self.process_index(&name)?;
            self.processes[index].enabled = true;
            self.processes[index].state = ProcessState::Pending;
            self.processes[index].detail = None;
            self.processes[index].next_restart = None;
        }
        self.start_runnable()
    }

    fn shutdown_all(&mut self) {
        let order = self.started_order.iter().rev().cloned().collect::<Vec<_>>();
        let mut stopped = BTreeSet::new();
        for name in order {
            if !stopped.insert(name.clone()) {
                continue;
            }
            if let Ok(index) = self.process_index(&name) {
                let (stop_signal, timeout) = self.stop_settings(index);
                self.processes[index].enabled = false;
                stop_managed_process(&mut self.processes[index], stop_signal, timeout);
            }
        }
        for process in &mut self.processes {
            process.cancel_probe();
            process.enabled = false;
            if matches!(
                process.state,
                ProcessState::Pending | ProcessState::Blocked | ProcessState::Restarting
            ) {
                process.state = ProcessState::Stopped;
                process.detail = Some("project stopped".into());
            }
        }
    }

    fn finish_output(&mut self) {
        self.output_sender.take();
        if let Some(writer) = self.output_writer.take() {
            let _ = writer.join();
        }
    }

    fn stop_settings(&self, index: usize) -> (Signal, Duration) {
        let stop = self.processes[index].config.stop.as_ref().or(self
            .config
            .defaults
            .stop
            .as_ref());
        let signal = stop
            .map(|stop| parse_signal(&stop.signal))
            .unwrap_or(Signal::SIGTERM);
        let timeout = stop
            .and_then(|stop| humantime::parse_duration(&stop.timeout).ok())
            .unwrap_or(Duration::from_secs(5));
        (signal, timeout)
    }

    fn validate_process_names(&self, names: &[String]) -> Result<(), SupervisorError> {
        for name in names {
            if !self.processes.iter().any(|process| process.name == *name) {
                return Err(SupervisorError::ProcessNotFound(name.clone()));
            }
        }
        Ok(())
    }

    fn process_index(&self, name: &str) -> Result<usize, SupervisorError> {
        self.processes
            .iter()
            .position(|process| process.name == name)
            .ok_or_else(|| SupervisorError::ProcessNotFound(name.into()))
    }

    fn process_mut(&mut self, name: &str) -> Result<&mut ManagedProcess, SupervisorError> {
        let index = self.process_index(name)?;
        Ok(&mut self.processes[index])
    }

    fn all_enabled_processes_finished(&self) -> bool {
        self.processes.iter().all(|process| {
            !process.enabled
                || matches!(
                    process.state,
                    ProcessState::Completed | ProcessState::Stopped
                )
        })
    }
}

fn selected_process_closure(
    config: &LoadedConfig,
    selected: &[String],
) -> Result<BTreeSet<String>, SupervisorError> {
    if selected.is_empty() {
        return Ok(config.processes.keys().cloned().collect());
    }
    process_name_closure(config, selected)
}

fn process_name_closure(
    config: &LoadedConfig,
    selected: &[String],
) -> Result<BTreeSet<String>, SupervisorError> {
    fn add(
        config: &LoadedConfig,
        name: &str,
        output: &mut BTreeSet<String>,
    ) -> Result<(), SupervisorError> {
        let process = config
            .processes
            .get(name)
            .ok_or_else(|| SupervisorError::ProcessNotFound(name.into()))?;
        if !output.insert(name.into()) {
            return Ok(());
        }
        for dependency in process.depends_on.keys() {
            add(config, dependency, output)?;
        }
        Ok(())
    }

    let mut output = BTreeSet::new();
    for name in selected {
        add(config, name, &mut output)?;
    }
    Ok(output)
}

fn condition_satisfied(state: ProcessState, condition: DependencyCondition) -> bool {
    match condition {
        DependencyCondition::Ready => matches!(
            state,
            ProcessState::Ready | ProcessState::Running | ProcessState::Completed
        ),
        DependencyCondition::CompletedSuccessfully => state == ProcessState::Completed,
    }
}

fn condition_label(condition: DependencyCondition) -> &'static str {
    match condition {
        DependencyCondition::Ready => "ready",
        DependencyCondition::CompletedSuccessfully => "completed_successfully",
    }
}

fn probe_label(kind: crate::config::ProbeType) -> &'static str {
    use crate::config::ProbeType;
    match kind {
        ProbeType::Tcp => "tcp",
        ProbeType::Tcp4 => "tcp4",
        ProbeType::Tcp6 => "tcp6",
        ProbeType::Http => "http",
        ProbeType::Https => "https",
        ProbeType::Unix => "unix",
        ProbeType::File => "file",
        ProbeType::Command => "command",
    }
}

fn project_root(config: &LoadedConfig) -> Result<PathBuf, SupervisorError> {
    let configured = config
        .project
        .path
        .as_deref()
        .ok_or_else(|| SupervisorError::ProjectRootMissing(config.project.id.clone()))?;
    let root = if configured == "~" {
        std::env::var_os("HOME").map(PathBuf::from)
    } else if let Some(remainder) = configured.strip_prefix("~/") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(remainder))
    } else {
        let path = PathBuf::from(configured);
        if path.is_absolute() {
            Some(path)
        } else {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(path))
        }
    };
    root.ok_or_else(|| SupervisorError::ProjectRootMissing(config.project.id.clone()))
}

fn register_instance(
    config: &LoadedConfig,
    root: &Path,
) -> Result<(UnixListener, Registration), SupervisorError> {
    let runtime_directory = runtime::runtime_directory()?;
    let instance = instance_directory(&runtime_directory, &config.project.id);
    let project_lock = match runtime::try_project_lock(&runtime_directory, &config.project.id)? {
        Some(lock) => lock,
        None => {
            if let Ok(existing) = runtime::read_metadata(&metadata_path(&instance)) {
                if runtime::send_request(
                    &existing,
                    &ControlRequest::Ping {
                        version: PROTOCOL_VERSION,
                    },
                )
                .is_ok()
                {
                    return Err(SupervisorError::AlreadyRunning {
                        project: config.project.id.clone(),
                        pid: existing.supervisor_pid,
                    });
                }
            }
            return Err(SupervisorError::ProjectUnresponsive {
                project: config.project.id.clone(),
            });
        }
    };
    if instance.exists() {
        fs::remove_dir_all(&instance).map_err(|source| SupervisorError::PrepareInstance {
            path: instance.clone(),
            source,
        })?;
    }
    fs::create_dir(&instance).map_err(|source| SupervisorError::PrepareInstance {
        path: instance.clone(),
        source,
    })?;
    fs::set_permissions(&instance, fs::Permissions::from_mode(0o700)).map_err(|source| {
        SupervisorError::PrepareInstance {
            path: instance.clone(),
            source,
        }
    })?;
    let registration = Registration {
        instance_directory: instance.clone(),
        _lock: project_lock,
    };
    let socket = socket_path(&instance);
    let listener = UnixListener::bind(&socket).map_err(|source| SupervisorError::BindSocket {
        path: socket.clone(),
        source,
    })?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).map_err(|source| {
        SupervisorError::BindSocket {
            path: socket.clone(),
            source,
        }
    })?;
    let metadata = InstanceMetadata {
        protocol_version: PROTOCOL_VERSION,
        project_id: config.project.id.clone(),
        project_name: config.project.display_name().into(),
        project_root: root.to_path_buf(),
        config_path: config.source.clone(),
        supervisor_pid: std::process::id(),
        started_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        socket,
    };
    write_metadata(&metadata_path(&instance), &metadata)?;
    Ok((listener, registration))
}

fn forward_output<R>(name: String, reader: R, stderr: bool, sender: SyncSender<OutputLine>)
where
    R: std::io::Read + Send + 'static,
{
    thread::spawn(move || {
        let color = 31 + name.bytes().fold(0_u8, u8::wrapping_add) % 6;
        let terminal = if stderr {
            std::io::stderr().is_terminal()
        } else {
            std::io::stdout().is_terminal()
        };
        let prefix: Arc<[u8]> = if terminal {
            format!("\x1b[1;{color}m{name}\x1b[0m | ")
        } else {
            format!("{name} | ")
        }
        .into_bytes()
        .into();
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            let done = match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => false,
                Err(_) if line.is_empty() => break,
                Err(_) => true,
            };
            if sender
                .send(OutputLine {
                    prefix: Arc::clone(&prefix),
                    line: std::mem::take(&mut line),
                    stderr,
                })
                .is_err()
                || done
            {
                break;
            }
        }
    });
}

fn write_output(receiver: Receiver<OutputLine>) {
    while let Ok(output) = receiver.recv() {
        let result = if output.stderr {
            write_prefixed_line(std::io::stderr().lock(), &output.prefix, &output.line)
        } else {
            write_prefixed_line(std::io::stdout().lock(), &output.prefix, &output.line)
        };
        if result.is_err() {
            break;
        }
    }
}

fn terminal_stream() -> std::io::Result<(Stdio, File)> {
    let size = Winsize {
        ws_row: 24,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let terminal = openpty(Some(&size), None).map_err(std::io::Error::from)?;
    Ok((Stdio::from(terminal.slave), File::from(terminal.master)))
}

fn write_prefixed_line(mut output: impl Write, prefix: &[u8], line: &[u8]) -> std::io::Result<()> {
    output.write_all(prefix)?;
    output.write_all(line)?;
    if !line.ends_with(b"\n") {
        output.write_all(b"\n")?;
    }
    output.flush()
}

fn stop_managed_process(process: &mut ManagedProcess, stop_signal: Signal, timeout: Duration) {
    process.cancel_probe();
    process.next_restart = None;
    let Some(pid) = process.pid else {
        process.state = ProcessState::Stopped;
        process.detail = Some("not running".into());
        return;
    };
    process.state = ProcessState::Stopping;
    process.detail = Some(format!("sending {stop_signal:?}"));
    system_log(format_args!("stopping '{}' with pid {pid}", process.name));
    let group = Pid::from_raw(pid as i32);
    let _ = signal::killpg(group, stop_signal);
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        if let Some(child) = process.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    process.child = None;
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        if !process_group_exists(group) {
            process.pid = None;
            process.state = ProcessState::Stopped;
            process.detail = Some("stopped gracefully".into());
            return;
        }
        thread::sleep(LOOP_INTERVAL);
    }
    let _ = signal::killpg(group, Signal::SIGKILL);
    if let Some(mut child) = process.child.take() {
        let _ = child.wait();
    }
    wait_for_process_group_exit(group);
    process.pid = None;
    process.state = ProcessState::Stopped;
    process.detail = Some(format!("killed after {timeout:?}"));
}

fn kill_process_group(group: Pid) {
    let _ = signal::killpg(group, Signal::SIGKILL);
    wait_for_process_group_exit(group);
}

fn wait_for_process_group_exit(group: Pid) {
    let deadline = Instant::now()
        .checked_add(PROCESS_GROUP_EXIT_TIMEOUT)
        .unwrap_or_else(Instant::now);
    while process_group_exists(group) && Instant::now() < deadline {
        thread::sleep(LOOP_INTERVAL);
    }
}

fn process_group_exists(group: Pid) -> bool {
    !matches!(
        signal::kill(Pid::from_raw(-group.as_raw()), None),
        Err(Errno::ESRCH)
    )
}

fn parse_signal(value: &str) -> Signal {
    match value
        .trim_start_matches("SIG")
        .to_ascii_uppercase()
        .as_str()
    {
        "ABRT" => Signal::SIGABRT,
        "HUP" => Signal::SIGHUP,
        "INT" => Signal::SIGINT,
        "KILL" => Signal::SIGKILL,
        "QUIT" => Signal::SIGQUIT,
        "STOP" => Signal::SIGSTOP,
        "USR1" => Signal::SIGUSR1,
        "USR2" => Signal::SIGUSR2,
        _ => Signal::SIGTERM,
    }
}

fn request_version(request: &ControlRequest) -> u32 {
    match request {
        ControlRequest::Ping { version }
        | ControlRequest::Status { version }
        | ControlRequest::StartProcesses { version, .. }
        | ControlRequest::StopProcesses { version, .. }
        | ControlRequest::RestartProcesses { version, .. }
        | ControlRequest::Shutdown { version } => *version,
    }
}

fn describe_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || "terminated by a signal".into(),
        |code| format!("exit code {code}"),
    )
}

fn system_log(arguments: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout().lock(), "system | {arguments}");
}

fn system_error(arguments: std::fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "system | {arguments}");
}
