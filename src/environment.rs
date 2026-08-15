use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("cannot load environment file {path}: {source}")]
    Load {
        path: PathBuf,
        source: dotenvy::Error,
    },
}

pub fn load_environment(
    root: &Path,
    project_files: &[String],
    process_files: &[String],
    process_values: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, EnvironmentError> {
    let mut values = BTreeMap::new();
    for file in project_files.iter().chain(process_files) {
        let path = PathBuf::from(file);
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        let entries = dotenvy::from_path_iter(&path).map_err(|source| EnvironmentError::Load {
            path: path.clone(),
            source,
        })?;
        for entry in entries {
            let (name, value) = entry.map_err(|source| EnvironmentError::Load {
                path: path.clone(),
                source,
            })?;
            values.insert(name, value);
        }
    }
    values.extend(process_values.clone());
    Ok(values)
}

pub fn expand_environment(input: &str, overrides: &BTreeMap<String, String>) -> String {
    let bytes = input.as_bytes();
    let mut result = String::with_capacity(input.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$'
            && bytes.get(index + 1) == Some(&b'{')
            && input[index + 2..].find('}').is_some()
        {
            let relative_end = input[index + 2..].find('}').unwrap();
            let end = index + 2 + relative_end;
            let name = &input[index + 2..end];
            if let Some(value) = overrides.get(name).cloned().or_else(|| env::var(name).ok()) {
                result.push_str(&value);
            }
            index = end + 1;
        } else {
            let character = input[index..]
                .chars()
                .next()
                .expect("index is on a character boundary");
            result.push(character);
            index += character.len_utf8();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_values_from_process_environment() {
        let values = BTreeMap::from([("PORT".into(), "4321".into())]);
        assert_eq!(
            expand_environment("http://127.0.0.1:${PORT}/ready", &values),
            "http://127.0.0.1:4321/ready"
        );
    }
}
