use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{ConfigError, ConfigRepository, LoadedConfig};

#[derive(Debug, Clone)]
pub struct Resolution {
    pub config: LoadedConfig,
    pub reason: String,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MatchScore {
    kind: u8,
    specificity: usize,
}

#[derive(Debug)]
struct Candidate {
    config: LoadedConfig,
    score: MatchScore,
    reason: String,
    project_root: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct GitContext {
    pub(crate) root: Option<PathBuf>,
    remotes: Vec<String>,
    pub(crate) primary_remote: Option<String>,
}

pub fn resolve_current(
    repository: &ConfigRepository,
    current_directory: &Path,
    explicit: Option<&Path>,
) -> Result<Resolution, ConfigError> {
    if let Some(id_or_path) = explicit {
        let config = repository.load_one(id_or_path)?;
        let git = inspect_git(current_directory);
        let project_root = configured_project_path(&config)
            .or(git.root)
            .unwrap_or_else(|| current_directory.to_path_buf());
        return Ok(Resolution {
            reason: format!("explicit configuration '{}'", id_or_path.display()),
            config,
            project_root,
        });
    }

    let current_directory = current_directory
        .canonicalize()
        .unwrap_or_else(|_| current_directory.to_path_buf());
    let git = inspect_git(&current_directory);
    let mut candidates = Vec::new();

    for config in repository.load_all()? {
        if let Some((score, reason, project_root)) = best_match(&config, &current_directory, &git) {
            candidates.push(Candidate {
                config,
                score,
                reason,
                project_root,
            });
        }
    }

    let Some(best_score) = candidates.iter().map(|candidate| candidate.score).max() else {
        return Err(ConfigError::ProjectNotFound {
            directory: current_directory,
        });
    };
    candidates.retain(|candidate| candidate.score == best_score);
    candidates.sort_by(|left, right| left.config.project.id.cmp(&right.config.project.id));

    if candidates.len() > 1 {
        return Err(ConfigError::AmbiguousProject {
            directory: current_directory,
            projects: candidates
                .iter()
                .map(|candidate| candidate.config.project.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        });
    }

    let candidate = candidates.pop().expect("one matching candidate remains");
    Ok(Resolution {
        config: candidate.config,
        reason: candidate.reason,
        project_root: candidate.project_root,
    })
}

fn best_match(
    config: &LoadedConfig,
    current_directory: &Path,
    git: &GitContext,
) -> Option<(MatchScore, String, PathBuf)> {
    if let Some(configured_path) = config.project.path.as_deref() {
        if let Some(project_path) = expand_project_path(configured_path) {
            let project_path = project_path
                .canonicalize()
                .unwrap_or_else(|_| project_path.to_path_buf());
            if current_directory.starts_with(&project_path) {
                return Some((
                    MatchScore {
                        kind: 3,
                        specificity: project_path.components().count(),
                    },
                    format!("current directory is inside {}", project_path.display()),
                    project_path,
                ));
            }
        }
    }

    for configured_remote in &config.project.git {
        let configured_remote = normalize_git_url(configured_remote);
        if let Some(actual_remote) = git
            .remotes
            .iter()
            .find(|actual| **actual == configured_remote)
        {
            return Some((
                MatchScore {
                    kind: 2,
                    specificity: 0,
                },
                format!("Git remote matches {actual_remote}"),
                git.root
                    .clone()
                    .unwrap_or_else(|| current_directory.to_path_buf()),
            ));
        }
    }

    let current_name = current_directory.file_name().and_then(|name| name.to_str());
    let git_name = git
        .root
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str());
    let mut configured_names = vec![config.project.id.as_str()];
    if let Some(name) = config.project.name.as_deref() {
        configured_names.push(name);
    }
    configured_names.extend(config.project.aliases.iter().map(String::as_str));

    for actual_name in [git_name, current_name].into_iter().flatten() {
        if configured_names.contains(&actual_name) {
            return Some((
                MatchScore {
                    kind: 1,
                    specificity: 0,
                },
                format!("project name matches '{actual_name}'"),
                git.root
                    .clone()
                    .unwrap_or_else(|| current_directory.to_path_buf()),
            ));
        }
    }

    None
}

fn configured_project_path(config: &LoadedConfig) -> Option<PathBuf> {
    config
        .project
        .path
        .as_deref()
        .and_then(expand_project_path)
        .map(|path| path.canonicalize().unwrap_or(path))
}

fn expand_project_path(path: &str) -> Option<PathBuf> {
    if path == "~" {
        return env::var_os("HOME").map(PathBuf::from);
    }
    if let Some(remainder) = path.strip_prefix("~/") {
        return env::var_os("HOME").map(|home| PathBuf::from(home).join(remainder));
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Some(path)
    } else {
        env::var_os("HOME").map(|home| PathBuf::from(home).join(path))
    }
}

pub(crate) fn inspect_git(current_directory: &Path) -> GitContext {
    let root_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(current_directory)
        .output();
    let Ok(root_output) = root_output else {
        return GitContext::default();
    };
    if !root_output.status.success() {
        return GitContext::default();
    }
    let root = PathBuf::from(String::from_utf8_lossy(&root_output.stdout).trim());

    let remote_names = Command::new("git")
        .arg("remote")
        .current_dir(&root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();

    let mut remotes = Vec::new();
    let mut primary_remote = None;
    for name in remote_names.lines().filter(|name| !name.trim().is_empty()) {
        let output = Command::new("git")
            .args(["remote", "get-url", "--all", name])
            .current_dir(&root)
            .output();
        if let Ok(output) = output {
            if output.status.success() {
                let urls = String::from_utf8_lossy(&output.stdout);
                if let Some(first) = urls.lines().next() {
                    if name == "origin" || primary_remote.is_none() {
                        primary_remote = Some(normalize_git_url(first));
                    }
                }
                remotes.extend(urls.lines().map(normalize_git_url));
            }
        }
    }

    GitContext {
        root: Some(root),
        remotes,
        primary_remote,
    }
}

pub fn normalize_git_url(input: &str) -> String {
    let input = input.trim().trim_end_matches('/');

    let (authority, path) = if let Some((_, remainder)) = input.split_once("://") {
        remainder
            .split_once('/')
            .map_or((remainder, ""), |(authority, path)| (authority, path))
    } else if let Some((authority, path)) = input.split_once(':') {
        if !authority.contains('/') {
            (authority, path)
        } else {
            ("", input)
        }
    } else {
        input
            .split_once('/')
            .map_or(("", input), |(authority, path)| (authority, path))
    };

    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
        .to_ascii_lowercase();
    let path = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or_else(|| path.trim_start_matches('/').trim_end_matches('/'));

    if authority.is_empty() {
        path.to_string()
    } else if path.is_empty() {
        authority
    } else {
        format!("{authority}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_common_git_remote_forms() {
        let expected = "github.com/acme/shop";
        assert_eq!(normalize_git_url("git@github.com:acme/shop.git"), expected);
        assert_eq!(
            normalize_git_url("ssh://git@github.com/acme/shop.git"),
            expected
        );
        assert_eq!(normalize_git_url("https://github.com/acme/shop/"), expected);
        assert_eq!(normalize_git_url("github.com/acme/shop"), expected);
    }
}
