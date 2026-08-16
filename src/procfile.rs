use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use thiserror::Error;

use crate::config::{
    sanitize_identifier, ConfigError, ConfigFile, DefaultsConfig, LoadedConfig, ProcessConfig,
    ProcessMode, ProjectConfig,
};

#[derive(Debug, Error)]
pub enum ProcfileError {
    #[error("cannot read Procfile {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid Procfile entry at {path}:{line}: expected NAME: COMMAND")]
    InvalidEntry { path: PathBuf, line: usize },
    #[error("Procfile {0} contains no processes")]
    Empty(PathBuf),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot encode converted configuration: {0}")]
    Encode(serde_yaml::Error),
}

pub fn load_procfile(path: &Path, project_id: Option<&str>) -> Result<LoadedConfig, ProcfileError> {
    let contents = fs::read_to_string(path).map_err(|source| ProcfileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .unwrap_or_else(|_| {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        });
    let inferred = root
        .file_name()
        .and_then(|name| name.to_str())
        .map(sanitize_identifier)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "procfile".into());
    let id = project_id.map(String::from).unwrap_or(inferred);
    let mut processes = IndexMap::new();
    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, command)) = line.split_once(':') else {
            return Err(ProcfileError::InvalidEntry {
                path: path.to_path_buf(),
                line: index + 1,
            });
        };
        let name = name.trim();
        let command = command.trim();
        if name.is_empty() || command.is_empty() {
            return Err(ProcfileError::InvalidEntry {
                path: path.to_path_buf(),
                line: index + 1,
            });
        }
        processes.insert(
            name.into(),
            ProcessConfig {
                command: command.into(),
                mode: ProcessMode::Service,
                color: None,
                depends_on: IndexMap::new(),
                readiness: None,
                restart: None,
                stop: None,
                working_directory: None,
                env: BTreeMap::new(),
                env_files: Vec::new(),
            },
        );
    }
    if processes.is_empty() {
        return Err(ProcfileError::Empty(path.to_path_buf()));
    }
    let env_files = root
        .join(".env")
        .is_file()
        .then(|| vec![".env".into()])
        .unwrap_or_default();
    LoadedConfig::from_config(
        path.to_path_buf(),
        ConfigFile {
            version: 1,
            project: ProjectConfig {
                id: id.clone(),
                name: Some(id),
                path: Some(root.to_string_lossy().into_owned()),
                git: Vec::new(),
                aliases: Vec::new(),
            },
            env_files,
            defaults: DefaultsConfig::default(),
            processes,
        },
    )
    .map_err(ProcfileError::Config)
}

pub fn convert_procfile(path: &Path, project_id: Option<&str>) -> Result<String, ProcfileError> {
    let loaded = load_procfile(path, project_id)?;
    serde_yaml::to_string(&loaded.to_config_file()).map_err(ProcfileError::Encode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_standard_procfile_entries_in_order() {
        let directory = TempDir::new().expect("temporary directory");
        let path = directory.path().join("Procfile");
        fs::write(&path, "web: run-web\nworker: run-worker\n").unwrap();
        let config = load_procfile(&path, Some("demo")).expect("valid Procfile");
        assert_eq!(
            config.processes.keys().collect::<Vec<_>>(),
            vec!["web", "worker"]
        );
    }
}
