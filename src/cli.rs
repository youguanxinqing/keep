use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use thiserror::Error;

use crate::config::{
    sanitize_identifier, validate_identifier, ConfigError, ConfigRepository, LoadedConfig,
};
use crate::procfile::{convert_procfile, load_procfile, ProcfileError};
use crate::resolver::{inspect_git, resolve_current};
use crate::runtime::{
    find_metadata, instance_directory, list_metadata, runtime_directory, send_request,
    try_project_lock, ControlRequest, InstanceMetadata, ProjectStatus, RuntimeError,
    PROTOCOL_VERSION,
};
use crate::supervisor::{Supervisor, SupervisorError};

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    Procfile(#[from] ProcfileError),
    #[error("invalid target '{0}'; expected PROJECT or PROJECT/PROCESS")]
    InvalidTarget(String),
    #[error("cannot read configuration {path}: {source}")]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("doctor found one or more required checks failing")]
    DoctorFailed,
    #[error("cannot determine a project ID from {0}; pass --project <ID>")]
    ProjectIdUnavailable(PathBuf),
    #[error("configuration already exists: {0}")]
    ConfigAlreadyExists(PathBuf),
    #[error("cannot create configuration directory {path}: {source}")]
    CreateConfigDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write configuration {path}: {source}")]
    WriteConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot encode configuration template: {0}")]
    EncodeConfigTemplate(serde_json::Error),
}

#[derive(Debug, Parser)]
#[command(
    name = "keep",
    version,
    about = "A project-aware process supervisor for local development"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start and supervise a project, or start stopped processes in a running project.
    Start(StartArgs),
    /// List processes from every running keep project.
    Ls(ListArgs),
    /// Show detailed runtime status.
    Status(StatusArgs),
    /// Stop running projects or processes.
    Stop(StopArgs),
    /// Restart running projects or processes.
    Restart(RestartArgs),
    /// Gracefully shut down one project's supervisor.
    Quit(QuitArgs),
    /// Create, inspect, and validate keep configuration files.
    Config(ConfigArgs),
    /// Run or convert a Procfile through explicit compatibility mode.
    Procfile(ProcfileArgs),
    /// Check the local keep installation and configuration.
    Doctor,
}

#[derive(Debug, Args)]
struct StartArgs {
    /// Select a configuration explicitly instead of matching the current directory.
    #[arg(long, value_name = "ID_OR_PATH")]
    config: Option<PathBuf>,
    /// Start only these processes and their dependencies.
    processes: Vec<String>,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Limit output to one project ID.
    project: Option<String>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Project ID or PROJECT/PROCESS. Lists everything when omitted.
    target: Option<String>,
}

#[derive(Debug, Args)]
struct StopArgs {
    /// Stop every running keep project.
    #[arg(long, conflicts_with = "targets")]
    all: bool,
    /// Project IDs or PROJECT/PROCESS targets.
    targets: Vec<String>,
}

#[derive(Debug, Args)]
struct RestartArgs {
    /// Project IDs or PROJECT/PROCESS targets. Defaults to the current project.
    targets: Vec<String>,
}

#[derive(Debug, Args)]
struct QuitArgs {
    /// Project ID.
    project: String,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Args)]
struct ProcfileArgs {
    #[command(subcommand)]
    command: ProcfileCommand,
}

#[derive(Debug, Subcommand)]
enum ProcfileCommand {
    /// Start processes from a Procfile in compatibility mode.
    Start(ProcfileStartArgs),
    /// Convert a Procfile to native version 1 YAML on stdout.
    Convert(ProcfileConvertArgs),
}

#[derive(Debug, Args)]
struct ProcfileStartArgs {
    /// Procfile path.
    #[arg(long, short = 'f', default_value = "Procfile")]
    file: PathBuf,
    /// Stable project ID used by global runtime commands.
    #[arg(long)]
    project: Option<String>,
}

#[derive(Debug, Args)]
struct ProcfileConvertArgs {
    /// Procfile path.
    #[arg(long, short = 'f', default_value = "Procfile")]
    file: PathBuf,
    /// Project ID written to the converted configuration.
    #[arg(long)]
    project: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Create a minimal native configuration for the current project.
    Init(ConfigInitArgs),
    /// List all valid project configurations.
    List,
    /// Display a configuration selected by ID/path or the current directory.
    Show(ConfigSelectionArgs),
    /// Parse and validate project configurations.
    Validate(ValidateArgs),
    /// Explain which configuration matches the current project.
    Resolve(ConfigSelectionArgs),
}

#[derive(Debug, Args)]
struct ConfigInitArgs {
    /// Write keep.yaml in the current Git root instead of the global configuration directory.
    #[arg(long)]
    local: bool,
    /// Stable project ID. Defaults to the current Git root or directory name.
    #[arg(long, value_name = "ID")]
    project: Option<String>,
}

#[derive(Debug, Args)]
struct ValidateArgs {
    /// Validate every YAML file in the keep configuration directory.
    #[arg(long, conflicts_with = "config")]
    all: bool,
    /// Validate one configuration by project ID or file path.
    #[arg(long, value_name = "ID_OR_PATH", conflicts_with = "all")]
    config: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ConfigSelectionArgs {
    /// Configuration project ID or file path.
    #[arg(value_name = "ID_OR_PATH", conflicts_with = "config")]
    selector: Option<PathBuf>,
    /// Select one configuration explicitly instead of matching the current directory.
    #[arg(long, value_name = "ID_OR_PATH")]
    config: Option<PathBuf>,
}

impl ConfigSelectionArgs {
    fn explicit(&self) -> Option<&Path> {
        self.config.as_deref().or(self.selector.as_deref())
    }
}

#[derive(Debug)]
struct Target {
    project: String,
    process: Option<String>,
}

pub fn run() -> Result<(), CliError> {
    match Cli::parse().command {
        Command::Start(args) => run_start(args),
        Command::Ls(args) => run_list(args),
        Command::Status(args) => run_status(args),
        Command::Stop(args) => run_stop(args),
        Command::Restart(args) => run_restart(args),
        Command::Quit(args) => run_quit(args),
        Command::Config(args) => run_config(ConfigRepository::from_environment()?, args.command),
        Command::Procfile(args) => run_procfile(args.command),
        Command::Doctor => run_doctor(),
    }
}

fn run_procfile(command: ProcfileCommand) -> Result<(), CliError> {
    match command {
        ProcfileCommand::Start(args) => {
            let config = load_procfile(&args.file, args.project.as_deref())?;
            Supervisor::new(config, &[])?.run()?;
        }
        ProcfileCommand::Convert(args) => {
            print!("{}", convert_procfile(&args.file, args.project.as_deref())?);
        }
    }
    Ok(())
}

fn run_doctor() -> Result<(), CliError> {
    let repository = ConfigRepository::from_environment()?;
    let mut failed = false;
    match repository.load_all() {
        Ok(configs) => println!(
            "ok   configuration: {} valid file(s) in {}",
            configs.len(),
            repository.directory().display()
        ),
        Err(error) => {
            println!("fail configuration: {error}");
            failed = true;
        }
    }
    match runtime_directory() {
        Ok(path) => println!("ok   runtime: {}", path.display()),
        Err(error) => {
            println!("fail runtime: {error}");
            failed = true;
        }
    }
    match std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .status()
    {
        Ok(status) if status.success() => println!("ok   shell: sh"),
        Ok(status) => {
            println!("fail shell: exited with {status}");
            failed = true;
        }
        Err(error) => {
            println!("fail shell: {error}");
            failed = true;
        }
    }
    match std::process::Command::new("git").arg("--version").output() {
        Ok(output) if output.status.success() => println!(
            "ok   git: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        ),
        _ => println!("warn git: unavailable; Git remote matching is disabled"),
    }
    if failed {
        Err(CliError::DoctorFailed)
    } else {
        Ok(())
    }
}

fn run_start(args: StartArgs) -> Result<(), CliError> {
    let repository = ConfigRepository::from_environment()?;
    let current_directory = current_directory()?;
    let resolution = resolve_current(&repository, &current_directory, args.config.as_deref())?;
    let runtime = runtime_directory()?;
    if let Ok(metadata) = find_metadata(&runtime, &resolution.config.project.id) {
        if send_request(
            &metadata,
            &ControlRequest::Ping {
                version: PROTOCOL_VERSION,
            },
        )
        .is_ok()
        {
            if args.processes.is_empty() {
                return Err(SupervisorError::AlreadyRunning {
                    project: metadata.project_id,
                    pid: metadata.supervisor_pid,
                }
                .into());
            }
            send_request(
                &metadata,
                &ControlRequest::StartProcesses {
                    version: PROTOCOL_VERSION,
                    processes: args.processes.clone(),
                },
            )?;
            println!(
                "started {}",
                args.processes
                    .iter()
                    .map(|process| format!("{}/{process}", resolution.config.project.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok(());
        }
    }
    let mut config = resolution.config;
    config.project.path = Some(resolution.project_root.to_string_lossy().into_owned());
    Supervisor::new(config, &args.processes)?.run()?;
    Ok(())
}

fn run_list(args: ListArgs) -> Result<(), CliError> {
    let statuses = collect_statuses(args.project.as_deref())?;
    print_statuses(&statuses, None, false);
    Ok(())
}

fn run_status(args: StatusArgs) -> Result<(), CliError> {
    let target = args.target.as_deref().map(parse_target).transpose()?;
    let statuses = collect_statuses(target.as_ref().map(|target| target.project.as_str()))?;
    if let Some(target) = &target {
        if let Some(process) = &target.process {
            let found = statuses
                .iter()
                .flat_map(|project| &project.processes)
                .any(|candidate| candidate.name == *process);
            if !found {
                return Err(RuntimeError::ProcessNotRunning {
                    project: target.project.clone(),
                    process: process.clone(),
                }
                .into());
            }
        }
    }
    print_statuses(
        &statuses,
        target.as_ref().and_then(|target| target.process.as_deref()),
        true,
    );
    Ok(())
}

fn run_stop(args: StopArgs) -> Result<(), CliError> {
    let runtime = runtime_directory()?;
    let targets = if args.all {
        collect_statuses(None)?
            .into_iter()
            .map(|status| Target {
                project: status.id,
                process: None,
            })
            .collect()
    } else if args.targets.is_empty() {
        vec![Target {
            project: current_project_id_with_running_hint()?,
            process: None,
        }]
    } else {
        args.targets
            .iter()
            .map(|target| parse_target(target))
            .collect::<Result<Vec<_>, _>>()?
    };
    for target in targets {
        let metadata = find_metadata(&runtime, &target.project)?;
        let request = target.process.as_ref().map_or(
            ControlRequest::Shutdown {
                version: PROTOCOL_VERSION,
            },
            |process| ControlRequest::StopProcesses {
                version: PROTOCOL_VERSION,
                processes: vec![process.clone()],
            },
        );
        send_request(&metadata, &request)?;
        println!("stopped {}", format_target(&target));
    }
    Ok(())
}

fn run_restart(args: RestartArgs) -> Result<(), CliError> {
    let targets = if args.targets.is_empty() {
        vec![Target {
            project: current_project_id()?,
            process: None,
        }]
    } else {
        args.targets
            .iter()
            .map(|target| parse_target(target))
            .collect::<Result<Vec<_>, _>>()?
    };
    let runtime = runtime_directory()?;
    for target in targets {
        let metadata = find_metadata(&runtime, &target.project)?;
        send_request(
            &metadata,
            &ControlRequest::RestartProcesses {
                version: PROTOCOL_VERSION,
                processes: target.process.iter().cloned().collect(),
            },
        )?;
        println!("restarted {}", format_target(&target));
    }
    Ok(())
}

fn run_quit(args: QuitArgs) -> Result<(), CliError> {
    let project = args.project;
    let runtime = runtime_directory()?;
    let metadata = find_metadata(&runtime, &project)?;
    send_request(
        &metadata,
        &ControlRequest::Shutdown {
            version: PROTOCOL_VERSION,
        },
    )?;
    println!("quit {project}");
    Ok(())
}

fn run_config(repository: ConfigRepository, command: ConfigCommand) -> Result<(), CliError> {
    match command {
        ConfigCommand::Init(args) => return run_config_init(&repository, args),
        ConfigCommand::List => {
            let configs = repository.load_all()?;
            println!("ID\tNAME\tPATH\tCONFIG");
            for config in configs {
                println!(
                    "{}\t{}\t{}\t{}",
                    config.project.id,
                    config.project.display_name(),
                    config.project.path.as_deref().unwrap_or("-"),
                    config.source.display()
                );
            }
        }
        ConfigCommand::Show(args) => {
            let config = select_config(&repository, args.explicit())?;
            let contents =
                fs::read_to_string(&config.source).map_err(|source| CliError::ReadConfig {
                    path: config.source.clone(),
                    source,
                })?;
            print!("{contents}");
            if !contents.ends_with('\n') {
                println!();
            }
        }
        ConfigCommand::Validate(args) => {
            if args.all {
                let configs = repository.load_all()?;
                println!("validated {} configuration(s)", configs.len());
            } else {
                let config = select_config(&repository, args.config.as_deref())?;
                println!(
                    "validated project '{}' from {}",
                    config.project.id,
                    config.source.display()
                );
            }
        }
        ConfigCommand::Resolve(args) => {
            let current_directory = current_directory()?;
            let resolution = resolve_current(&repository, &current_directory, args.explicit())?;
            println!("selected: {}", resolution.config.source.display());
            println!("project: {}", resolution.config.project.id);
            println!("reason: {}", resolution.reason);
            println!("project root: {}", resolution.project_root.display());
            println!("working directory: {}", current_directory.display());
        }
    }
    Ok(())
}

fn run_config_init(repository: &ConfigRepository, args: ConfigInitArgs) -> Result<(), CliError> {
    let current = current_directory()?;
    let current = current.canonicalize().unwrap_or(current);
    let git = inspect_git(&current);
    let root = git.root.unwrap_or(current);
    let project = args.project.unwrap_or_else(|| {
        root.file_name()
            .and_then(|name| name.to_str())
            .map(sanitize_identifier)
            .unwrap_or_default()
    });
    if project.is_empty() {
        return Err(CliError::ProjectIdUnavailable(root));
    }

    let target = if args.local {
        root.join("keep.yaml")
    } else {
        repository.directory().join(format!("{project}.yaml"))
    };
    validate_identifier("project id", &project).map_err(|message| ConfigError::InvalidConfig {
        path: target.clone(),
        message,
    })?;
    let target_directory = target.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(target_directory).map_err(|source| CliError::CreateConfigDirectory {
        path: target_directory.to_path_buf(),
        source,
    })?;

    let project_match = if let Some(remote) = git.primary_remote {
        let remote = serde_json::to_string(&remote).map_err(CliError::EncodeConfigTemplate)?;
        format!("  git:\n    - {remote}\n")
    } else {
        let path = serde_json::to_string(root.to_string_lossy().as_ref())
            .map_err(CliError::EncodeConfigTemplate)?;
        format!("  path: {path}\n")
    };
    let contents = format!(
        "version: 1\n\nproject:\n  id: {project}\n{project_match}\nprocesses:\n  app:\n    command: echo \"configure this process\"\n"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::AlreadyExists {
                CliError::ConfigAlreadyExists(target.clone())
            } else {
                CliError::WriteConfig {
                    path: target.clone(),
                    source,
                }
            }
        })?;
    file.write_all(contents.as_bytes())
        .map_err(|source| CliError::WriteConfig {
            path: target.clone(),
            source,
        })?;
    LoadedConfig::load(&target)?;

    println!("created {}", target.display());
    if args.local {
        println!(
            "next: edit {}, then run `keep start --config {}`",
            target.display(),
            target.display()
        );
    } else {
        println!("next: edit {}, then run `keep start`", target.display());
    }
    Ok(())
}

fn select_config(
    repository: &ConfigRepository,
    explicit: Option<&Path>,
) -> Result<crate::config::LoadedConfig, CliError> {
    if let Some(explicit) = explicit {
        return Ok(repository.load_one(explicit)?);
    }
    Ok(resolve_current(repository, &current_directory()?, None)?.config)
}

fn collect_statuses(project_filter: Option<&str>) -> Result<Vec<ProjectStatus>, CliError> {
    let runtime = runtime_directory()?;
    let mut statuses = Vec::new();
    for metadata in list_metadata(&runtime)? {
        if project_filter.is_some_and(|project| project != metadata.project_id) {
            continue;
        }
        match query_status(&metadata) {
            Ok(status) => statuses.push(status),
            Err(error) => match try_project_lock(&runtime, &metadata.project_id)? {
                Some(_lock) => {
                    let stale = instance_directory(&runtime, &metadata.project_id);
                    let _ = fs::remove_dir_all(&stale);
                    eprintln!(
                        "keep: removed stale registration for '{}': {error}",
                        metadata.project_id
                    );
                }
                None => return Err(error),
            },
        }
    }
    if let Some(project) = project_filter {
        if statuses.is_empty() {
            return Err(RuntimeError::ProjectNotRunning(project.into()).into());
        }
    }
    Ok(statuses)
}

fn query_status(metadata: &InstanceMetadata) -> Result<ProjectStatus, CliError> {
    send_request(
        metadata,
        &ControlRequest::Status {
            version: PROTOCOL_VERSION,
        },
    )?
    .project
    .ok_or_else(|| {
        RuntimeError::InvalidResponse {
            project: metadata.project_id.clone(),
            message: "status response has no project".into(),
        }
        .into()
    })
}

fn print_statuses(statuses: &[ProjectStatus], process_filter: Option<&str>, detailed: bool) {
    if detailed {
        println!("PROJECT\tPROCESS\tPID\tSTATUS\tRESTARTS\tDETAIL\tROOT");
    } else {
        println!("PROJECT\tPROCESS\tPID\tSTATUS\tROOT");
    }
    for project in statuses {
        for process in &project.processes {
            if process_filter.is_some_and(|filter| filter != process.name) {
                continue;
            }
            let pid = process
                .pid
                .map_or_else(|| "-".into(), |pid| pid.to_string());
            if detailed {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    project.id,
                    process.name,
                    pid,
                    process.state,
                    process.restart_count,
                    process.detail.as_deref().unwrap_or("-"),
                    project.root.display()
                );
            } else {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    project.id,
                    process.name,
                    pid,
                    process.state,
                    project.root.display()
                );
            }
        }
    }
}

fn parse_target(value: &str) -> Result<Target, CliError> {
    let mut parts = value.split('/');
    let project = parts.next().unwrap_or_default();
    let process = parts.next();
    if project.is_empty()
        || process == Some("")
        || parts.next().is_some()
        || !project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        || process.is_some_and(|process| {
            !process
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err(CliError::InvalidTarget(value.into()));
    }
    Ok(Target {
        project: project.into(),
        process: process.map(String::from),
    })
}

fn format_target(target: &Target) -> String {
    target.process.as_ref().map_or_else(
        || target.project.clone(),
        |process| format!("{}/{process}", target.project),
    )
}

fn current_project_id() -> Result<String, CliError> {
    let repository = ConfigRepository::from_environment()?;
    Ok(resolve_current(&repository, &current_directory()?, None)?
        .config
        .project
        .id)
}

fn current_project_id_with_running_hint() -> Result<String, CliError> {
    match current_project_id() {
        Ok(project) => Ok(project),
        Err(error) => {
            if let Ok(statuses) = collect_statuses(None) {
                let projects = statuses
                    .iter()
                    .map(|status| status.id.as_str())
                    .collect::<Vec<_>>();
                if !projects.is_empty() {
                    eprintln!(
                        "keep: running projects: {}; pass one to `keep stop`",
                        projects.join(", ")
                    );
                }
            }
            Err(error)
        }
    }
}

fn current_directory() -> Result<PathBuf, ConfigError> {
    std::env::current_dir().map_err(ConfigError::CurrentDirectory)
}
