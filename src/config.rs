use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("HOME is unavailable; set KEEP_CONFIG_DIR explicitly")]
    HomeDirectoryUnavailable,

    #[error("configuration directory does not exist: {0}")]
    DirectoryNotFound(PathBuf),

    #[error("cannot read configuration directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("cannot read configuration {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("invalid YAML in {path}: {source}")]
    InvalidYaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },

    #[error("invalid configuration {path}: {message}")]
    InvalidConfig { path: PathBuf, message: String },

    #[error("duplicate project id '{id}' in {first} and {second}")]
    DuplicateProjectId {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("configuration not found: {0}")]
    ConfigNotFound(String),

    #[error("pass --all or --config <ID_OR_PATH> to select what to validate")]
    ValidationTargetRequired,

    #[error("cannot determine the current directory: {0}")]
    CurrentDirectory(std::io::Error),

    #[error("no keep configuration matches {directory}")]
    ProjectNotFound { directory: PathBuf },

    #[error("multiple keep configurations match {directory}: {projects}")]
    AmbiguousProject {
        directory: PathBuf,
        projects: String,
    },
}

#[derive(Debug, Clone)]
pub struct ConfigRepository {
    directory: PathBuf,
}

impl ConfigRepository {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn from_environment() -> Result<Self, ConfigError> {
        Ok(Self::new(config_dir_from_environment()?))
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn load_all(&self) -> Result<Vec<LoadedConfig>, ConfigError> {
        let paths = self.config_paths()?;
        let mut configs = Vec::with_capacity(paths.len());
        let mut ids = BTreeMap::<String, PathBuf>::new();

        for path in paths {
            let config = LoadedConfig::load(&path)?;
            if let Some(first) = ids.insert(config.project.id.clone(), path.clone()) {
                return Err(ConfigError::DuplicateProjectId {
                    id: config.project.id.clone(),
                    first,
                    second: path,
                });
            }
            configs.push(config);
        }

        Ok(configs)
    }

    pub fn load_one(&self, id_or_path: &Path) -> Result<LoadedConfig, ConfigError> {
        if id_or_path.components().count() > 1 || id_or_path.is_absolute() || id_or_path.is_file() {
            return LoadedConfig::load(id_or_path);
        }

        let requested = id_or_path.to_string_lossy();
        let configs = self.load_all()?;
        configs
            .into_iter()
            .find(|config| {
                config.project.id == requested
                    || config
                        .source
                        .file_name()
                        .is_some_and(|name| name == requested.as_ref())
                    || config
                        .source
                        .file_stem()
                        .is_some_and(|stem| stem == requested.as_ref())
            })
            .ok_or_else(|| ConfigError::ConfigNotFound(requested.into_owned()))
    }

    fn config_paths(&self) -> Result<Vec<PathBuf>, ConfigError> {
        if !self.directory.is_dir() {
            return Err(ConfigError::DirectoryNotFound(self.directory.clone()));
        }

        let entries =
            fs::read_dir(&self.directory).map_err(|source| ConfigError::ReadDirectory {
                path: self.directory.clone(),
                source,
            })?;

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ConfigError::ReadDirectory {
                path: self.directory.clone(),
                source,
            })?;
            let path = entry.path();
            let is_yaml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| matches!(extension, "yaml" | "yml"));
            if path.is_file() && is_yaml {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }
}

fn config_dir_from_environment() -> Result<PathBuf, ConfigError> {
    if let Some(directory) = env::var_os("KEEP_CONFIG_DIR") {
        return Ok(PathBuf::from(directory));
    }

    let home = env::var_os("HOME").ok_or(ConfigError::HomeDirectoryUnavailable)?;
    Ok(PathBuf::from(home).join(".config").join("keep"))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub version: u32,
    pub project: ProjectConfig,
    #[serde(default)]
    pub env_files: Vec<String>,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    pub processes: IndexMap<String, ProcessConfig>,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub source: PathBuf,
    pub version: u32,
    pub project: ProjectConfig,
    pub env_files: Vec<String>,
    pub defaults: DefaultsConfig,
    pub processes: IndexMap<String, ProcessConfig>,
}

impl LoadedConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        let input: ConfigFile =
            serde_yaml::from_str(&contents).map_err(|source| ConfigError::InvalidYaml {
                path: path.to_path_buf(),
                source,
            })?;
        Self::from_config(path.to_path_buf(), input)
    }

    pub fn from_config(source: PathBuf, input: ConfigFile) -> Result<Self, ConfigError> {
        validate(&input).map_err(|message| ConfigError::InvalidConfig {
            path: source.clone(),
            message,
        })?;

        Ok(Self {
            source,
            version: input.version,
            project: input.project,
            env_files: input.env_files,
            defaults: input.defaults,
            processes: input.processes,
        })
    }

    pub fn to_config_file(&self) -> ConfigFile {
        ConfigFile {
            version: self.version,
            project: self.project.clone(),
            env_files: self.env_files.clone(),
            defaults: self.defaults.clone(),
            processes: self.processes.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl ProjectConfig {
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    #[serde(default)]
    pub stop: Option<StopConfig>,
    #[serde(default)]
    pub restart: Option<RestartConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StopConfig {
    #[serde(default = "default_stop_signal")]
    pub signal: String,
    #[serde(default = "default_stop_timeout")]
    pub timeout: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestartConfig {
    #[serde(default)]
    pub policy: RestartPolicy,
    #[serde(default)]
    pub backoff: Option<String>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub command: String,
    #[serde(default)]
    pub mode: ProcessMode,
    #[serde(default)]
    pub depends_on: IndexMap<String, DependencyCondition>,
    #[serde(default)]
    pub readiness: Option<ReadinessConfig>,
    #[serde(default)]
    pub restart: Option<RestartConfig>,
    #[serde(default)]
    pub stop: Option<StopConfig>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_files: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessMode {
    #[default]
    Service,
    Task,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyCondition {
    Ready,
    CompletedSuccessfully,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadinessConfig {
    #[serde(rename = "type")]
    pub kind: ProbeType,
    pub target: String,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub attempt_timeout: Option<String>,
    #[serde(default)]
    pub startup_timeout: Option<String>,
    #[serde(default)]
    pub success_threshold: Option<u32>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub expected_status: Option<u16>,
    #[serde(default)]
    pub tls_ca: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeType {
    Tcp,
    Tcp4,
    Tcp6,
    Http,
    Https,
    Unix,
    File,
    Command,
}

fn validate(config: &ConfigFile) -> Result<(), String> {
    if config.version != SUPPORTED_VERSION {
        return Err(format!(
            "unsupported version {}; this build supports version {SUPPORTED_VERSION}",
            config.version
        ));
    }
    validate_identifier("project id", &config.project.id)?;
    if config.processes.is_empty() {
        return Err("at least one process is required".into());
    }
    if let Some(stop) = &config.defaults.stop {
        validate_stop("defaults", stop)?;
    }
    if let Some(restart) = &config.defaults.restart {
        validate_restart("defaults", restart)?;
    }

    for (name, process) in &config.processes {
        validate_identifier("process name", name)?;
        if process.command.trim().is_empty() {
            return Err(format!("process '{name}' has an empty command"));
        }
        if process
            .readiness
            .as_ref()
            .and_then(|probe| probe.success_threshold)
            == Some(0)
        {
            return Err(format!(
                "process '{name}' readiness success_threshold must be greater than zero"
            ));
        }
        if process.mode == ProcessMode::Task && process.readiness.is_some() {
            return Err(format!("task process '{name}' cannot configure readiness"));
        }
        if let Some(readiness) = &process.readiness {
            validate_readiness(name, readiness)?;
        }
        if let Some(stop) = &process.stop {
            validate_stop(&format!("process '{name}'"), stop)?;
        }
        if let Some(restart) = &process.restart {
            validate_restart(&format!("process '{name}'"), restart)?;
        }

        for (dependency, condition) in &process.depends_on {
            let Some(target) = config.processes.get(dependency) else {
                return Err(format!(
                    "process '{name}' depends on unknown process '{dependency}'"
                ));
            };
            if dependency == name {
                return Err(format!("process '{name}' cannot depend on itself"));
            }
            if *condition == DependencyCondition::CompletedSuccessfully
                && target.mode != ProcessMode::Task
            {
                return Err(format!(
                    "process '{name}' requires '{dependency}' to complete successfully, but '{dependency}' is not a task"
                ));
            }
        }
    }

    validate_acyclic(&config.processes)
}

fn validate_readiness(process: &str, readiness: &ReadinessConfig) -> Result<(), String> {
    if readiness.target.trim().is_empty() {
        return Err(format!("process '{process}' readiness target is empty"));
    }
    for (label, value) in [
        ("interval", readiness.interval.as_deref()),
        ("attempt_timeout", readiness.attempt_timeout.as_deref()),
        ("startup_timeout", readiness.startup_timeout.as_deref()),
    ] {
        if let Some(value) = value {
            validate_duration(&format!("process '{process}' readiness {label}"), value)?;
        }
    }
    if readiness.success_threshold == Some(0) {
        return Err(format!(
            "process '{process}' readiness success_threshold must be greater than zero"
        ));
    }
    if let Some(status) = readiness.expected_status {
        if !(100..=599).contains(&status) {
            return Err(format!(
                "process '{process}' readiness expected_status must be between 100 and 599"
            ));
        }
    }
    Ok(())
}

fn validate_stop(owner: &str, stop: &StopConfig) -> Result<(), String> {
    const SIGNALS: &[&str] = &[
        "ABRT", "HUP", "INT", "KILL", "QUIT", "STOP", "TERM", "USR1", "USR2",
    ];
    let signal = stop.signal.trim_start_matches("SIG").to_ascii_uppercase();
    if !SIGNALS.contains(&signal.as_str()) {
        return Err(format!(
            "{owner} has unsupported stop signal '{}'",
            stop.signal
        ));
    }
    validate_duration(&format!("{owner} stop timeout"), &stop.timeout)
}

fn validate_restart(owner: &str, restart: &RestartConfig) -> Result<(), String> {
    if let Some(backoff) = &restart.backoff {
        validate_duration(&format!("{owner} restart backoff"), backoff)?;
    }
    Ok(())
}

fn validate_duration(label: &str, value: &str) -> Result<(), String> {
    let duration = humantime::parse_duration(value)
        .map_err(|error| format!("{label} '{value}' is invalid: {error}"))?;
    if duration.is_zero() {
        return Err(format!("{label} must be greater than zero"));
    }
    if std::time::Instant::now().checked_add(duration).is_none() {
        return Err(format!("{label} is too large for this platform"));
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> Result<(), String> {
    let valid = !value.is_empty()
        && value.len() <= 48
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "{label} '{value}' must be at most 48 characters and contain only ASCII letters, digits, '_' or '-'"
        ))
    }
}

fn validate_acyclic(processes: &IndexMap<String, ProcessConfig>) -> Result<(), String> {
    fn visit<'a>(
        name: &'a str,
        processes: &'a IndexMap<String, ProcessConfig>,
        visiting: &mut Vec<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> Result<(), String> {
        if let Some(position) = visiting.iter().position(|candidate| *candidate == name) {
            let mut cycle = visiting[position..].to_vec();
            cycle.push(name);
            return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
        }
        if visited.contains(name) {
            return Ok(());
        }

        visiting.push(name);
        for dependency in processes[name].depends_on.keys() {
            visit(dependency, processes, visiting, visited)?;
        }
        visiting.pop();
        visited.insert(name);
        Ok(())
    }

    let mut visiting = Vec::new();
    let mut visited = BTreeSet::new();
    for name in processes.keys() {
        visit(name, processes, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn default_stop_signal() -> String {
    "TERM".into()
}

fn default_stop_timeout() -> String {
    "5s".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> ConfigFile {
        serde_yaml::from_str(yaml).expect("test YAML should parse")
    }

    #[test]
    fn accepts_a_valid_dependency_chain() {
        let config = parse(
            r#"
version: 1
project:
  id: shop
processes:
  database:
    command: postgres
  migrate:
    command: migrate
    mode: task
    depends_on:
      database: ready
  api:
    command: api
    depends_on:
      migrate: completed_successfully
"#,
        );

        assert_eq!(validate(&config), Ok(()));
    }

    #[test]
    fn rejects_dependency_cycles_with_the_cycle_path() {
        let config = parse(
            r#"
version: 1
project:
  id: shop
processes:
  api:
    command: api
    depends_on:
      worker: ready
  worker:
    command: worker
    depends_on:
      api: ready
"#,
        );

        assert_eq!(
            validate(&config),
            Err("dependency cycle: api -> worker -> api".into())
        );
    }

    #[test]
    fn rejects_completed_condition_for_a_service() {
        let config = parse(
            r#"
version: 1
project:
  id: shop
processes:
  database:
    command: postgres
  api:
    command: api
    depends_on:
      database: completed_successfully
"#,
        );

        assert_eq!(
            validate(&config),
            Err("process 'api' requires 'database' to complete successfully, but 'database' is not a task".into())
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = serde_yaml::from_str::<ConfigFile>(
            r#"
version: 1
project:
  id: shop
  typo: true
processes:
  api:
    command: api
"#,
        )
        .expect_err("unknown fields must fail");

        assert!(error.to_string().contains("unknown field `typo`"));
    }

    #[test]
    fn rejects_a_duration_that_cannot_be_represented_as_an_instant() {
        let config = parse(
            r#"
version: 1
project:
  id: demo
processes:
  api:
    command: run-api
    readiness:
      type: command
      target: "exit 0"
      startup_timeout: 18446744073709551615s
"#,
        );
        let error = LoadedConfig::from_config(PathBuf::from("huge.yaml"), config).unwrap_err();
        assert!(error.to_string().contains("too large"), "{error}");
    }
}
