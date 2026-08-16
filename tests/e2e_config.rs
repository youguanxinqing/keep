use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

const VALID_CONFIG: &str = r#"
version: 1
project:
  id: shop
  name: Shop
  path: /projects/shop
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
"#;

fn keep(config_dir: &Path, working_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(args)
        .env("KEEP_CONFIG_DIR", config_dir)
        .current_dir(working_dir)
        .output()
        .expect("keep should execute")
}

fn write_config(directory: &Path, name: &str, contents: &str) {
    fs::write(directory.join(name), contents).expect("configuration fixture should be written");
}

#[test]
fn help_and_version_are_available() {
    let directory = TempDir::new().expect("temporary directory");

    let help = keep(directory.path(), directory.path(), &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage: keep <COMMAND>"));

    let version = keep(directory.path(), directory.path(), &["--version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("keep 0.1.0"));
}

#[test]
fn config_list_works_from_an_unrelated_directory() {
    let config_dir = TempDir::new().expect("configuration directory");
    let working_dir = TempDir::new().expect("unrelated working directory");
    write_config(config_dir.path(), "shop.yaml", VALID_CONFIG);

    let output = keep(config_dir.path(), working_dir.path(), &["config", "list"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ID\tNAME\tPATH\tCONFIG"));
    assert!(stdout.contains("shop\tShop\t/projects/shop"));
    assert!(stdout.contains("shop.yaml"));
}

#[test]
fn config_validate_all_executes_the_real_binary() {
    let config_dir = TempDir::new().expect("configuration directory");
    write_config(config_dir.path(), "shop.yaml", VALID_CONFIG);

    let output = keep(
        config_dir.path(),
        config_dir.path(),
        &["config", "validate", "--all"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "validated 1 configuration(s)\n"
    );
}

#[test]
fn config_name_is_used_as_id_when_id_is_omitted() {
    let config_dir = TempDir::new().unwrap();
    let working_dir = TempDir::new().unwrap();
    write_config(
        config_dir.path(),
        "shop.yaml",
        r#"
version: 1
project:
  name: shop
processes:
  app:
    command: run-app
"#,
    );

    let validate = keep(
        config_dir.path(),
        working_dir.path(),
        &["config", "validate", "--all"],
    );
    assert!(
        validate.status.success(),
        "{}",
        String::from_utf8_lossy(&validate.stderr)
    );

    let list = keep(config_dir.path(), working_dir.path(), &["config", "list"]);
    assert!(list.status.success());
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("shop\tshop\t"),
        "{}",
        String::from_utf8_lossy(&list.stdout)
    );
}

#[test]
fn config_validate_one_by_project_id() {
    let config_dir = TempDir::new().expect("configuration directory");
    write_config(config_dir.path(), "different-file-name.yml", VALID_CONFIG);

    let output = keep(
        config_dir.path(),
        config_dir.path(),
        &["config", "validate", "--config", "shop"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("validated project 'shop'"));
    assert!(stdout.contains("different-file-name.yml"));
}

#[test]
fn config_validate_one_by_relative_file_path() {
    let config_dir = TempDir::new().expect("configuration directory");
    write_config(config_dir.path(), "shop.yml", VALID_CONFIG);

    let output = keep(
        config_dir.path(),
        config_dir.path(),
        &["config", "validate", "--config", "shop.yml"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("shop.yml"));
}

#[test]
fn config_validate_reports_a_dependency_cycle() {
    let config_dir = TempDir::new().expect("configuration directory");
    write_config(
        config_dir.path(),
        "cycle.yaml",
        r#"
version: 1
project:
  id: cycle
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

    let output = keep(
        config_dir.path(),
        config_dir.path(),
        &["config", "validate", "--all"],
    );

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dependency cycle: api -> worker -> api")
    );
}

#[test]
fn config_validate_defaults_to_the_current_project() {
    let config_dir = TempDir::new().expect("configuration directory");
    let project = TempDir::new().expect("project directory");
    write_config(
        config_dir.path(),
        "current.yaml",
        &format!(
            r#"
version: 1
project:
  id: current
  path: {}
processes:
  dev:
    command: dev
"#,
            project.path().display()
        ),
    );

    let output = keep(config_dir.path(), project.path(), &["config", "validate"]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("validated project 'current'"));
}

#[test]
fn config_show_prints_the_selected_source_file() {
    let config_dir = TempDir::new().expect("configuration directory");
    write_config(config_dir.path(), "shop.yaml", VALID_CONFIG);

    let output = keep(
        config_dir.path(),
        config_dir.path(),
        &["config", "show", "shop"],
    );

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), VALID_CONFIG);
}

#[test]
fn config_resolve_selects_the_longest_containing_project_path() {
    let config_dir = TempDir::new().expect("configuration directory");
    let workspace = TempDir::new().expect("project workspace");
    let nested_project = workspace.path().join("services").join("api");
    let working_directory = nested_project.join("src");
    fs::create_dir_all(&working_directory).expect("nested project directory");

    write_config(
        config_dir.path(),
        "workspace.yaml",
        &format!(
            r#"
version: 1
project:
  id: workspace
  path: {}
processes:
  dev:
    command: dev
"#,
            workspace.path().display()
        ),
    );
    write_config(
        config_dir.path(),
        "api.yaml",
        &format!(
            r#"
version: 1
project:
  id: api
  path: {}
processes:
  dev:
    command: dev
"#,
            nested_project.display()
        ),
    );

    let output = keep(
        config_dir.path(),
        &working_directory,
        &["config", "resolve"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project: api"));
    assert!(stdout.contains("reason: current directory is inside"));
}

#[test]
fn config_resolve_reports_ambiguous_name_matches() {
    let config_dir = TempDir::new().expect("configuration directory");
    let workspace = TempDir::new().expect("workspace directory");
    let working_directory = workspace.path().join("shared-name");
    fs::create_dir(&working_directory).expect("working directory");

    for id in ["alpha", "beta"] {
        write_config(
            config_dir.path(),
            &format!("{id}.yaml"),
            &format!(
                r#"
version: 1
project:
  id: {id}
  aliases: [shared-name]
processes:
  dev:
    command: dev
"#
            ),
        );
    }

    let output = keep(
        config_dir.path(),
        &working_directory,
        &["config", "resolve"],
    );

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("multiple keep configurations match"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("alpha, beta"));
}

#[test]
fn config_resolve_matches_the_project_name() {
    let config_dir = TempDir::new().expect("configuration directory");
    let workspace = TempDir::new().expect("workspace directory");
    let working_directory = workspace.path().join("checkout-api");
    fs::create_dir(&working_directory).expect("working directory");
    write_config(
        config_dir.path(),
        "checkout.yaml",
        r#"
version: 1
project:
  id: checkout
  name: checkout-api
processes:
  dev:
    command: dev
"#,
    );

    let output = keep(
        config_dir.path(),
        &working_directory,
        &["config", "resolve"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project: checkout"));
    assert!(stdout.contains("reason: project name matches 'checkout-api'"));
}

#[test]
fn config_resolve_normalizes_and_matches_git_remotes() {
    let config_dir = TempDir::new().expect("configuration directory");
    let repository = TempDir::new().expect("Git repository");
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(repository.path())
        .status()
        .expect("git should execute");
    assert!(init.success());
    let remote = Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:acme/checkout.git",
        ])
        .current_dir(repository.path())
        .status()
        .expect("git should execute");
    assert!(remote.success());
    write_config(
        config_dir.path(),
        "checkout.yaml",
        r#"
version: 1
project:
  id: configured-id
  git:
    - https://github.com/acme/checkout
processes:
  dev:
    command: dev
"#,
    );

    let output = keep(config_dir.path(), repository.path(), &["config", "resolve"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project: configured-id"));
    assert!(stdout.contains("reason: Git remote matches github.com/acme/checkout"));
}

#[test]
fn config_resolve_accepts_an_explicit_project_id() {
    let config_dir = TempDir::new().expect("configuration directory");
    let working_dir = TempDir::new().expect("unrelated working directory");
    write_config(config_dir.path(), "shop.yaml", VALID_CONFIG);

    let output = keep(
        config_dir.path(),
        working_dir.path(),
        &["config", "resolve", "--config", "shop"],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("project: shop"));
    assert!(stdout.contains("reason: explicit configuration 'shop'"));
}

#[test]
fn config_resolve_expands_a_relative_project_path_from_home() {
    let config_dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let project = home.path().join("projects/shop");
    let working_directory = project.join("src");
    fs::create_dir_all(&working_directory).unwrap();
    write_config(
        config_dir.path(),
        "shop.yaml",
        r#"
version: 1
project:
  id: shop
  path: projects/shop
processes:
  api:
    command: api
"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_keep"))
        .args(["config", "resolve"])
        .env("KEEP_CONFIG_DIR", config_dir.path())
        .env("HOME", home.path())
        .current_dir(&working_directory)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!(
        "project root: {}",
        project.canonicalize().unwrap().display()
    )));
}

#[test]
fn config_resolve_uses_repository_local_config_without_a_global_directory() {
    let workspace = TempDir::new().unwrap();
    let repository = workspace.path().join("shop");
    let nested = repository.join("services/api");
    let missing_config_dir = workspace.path().join("missing-global-config");
    fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    write_config(
        &repository,
        "keep.yaml",
        r#"
version: 1
project:
  name: shop
processes:
  app:
    command: run-app
"#,
    );

    let output = keep(&missing_config_dir, &nested, &["config", "resolve"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&format!(
        "selected: {}",
        repository
            .canonicalize()
            .unwrap()
            .join("keep.yaml")
            .display()
    )));
    assert!(stdout.contains("project: shop"));
    assert!(stdout.contains("reason: repository-local keep.yaml"));
}

#[test]
fn config_validate_all_rejects_duplicate_project_ids() {
    let config_dir = TempDir::new().unwrap();
    let working_dir = TempDir::new().unwrap();
    write_config(config_dir.path(), "one.yaml", VALID_CONFIG);
    write_config(
        config_dir.path(),
        "two.yaml",
        r#"
version: 1
project:
  name: shop
processes:
  app:
    command: run-app
"#,
    );

    let output = keep(
        config_dir.path(),
        working_dir.path(),
        &["config", "validate", "--all"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate project id 'shop'"));
}

#[test]
fn config_validate_rejects_unknown_fields_through_the_binary() {
    let config_dir = TempDir::new().unwrap();
    let working_dir = TempDir::new().unwrap();
    write_config(
        config_dir.path(),
        "typo.yaml",
        r#"
version: 1
project:
  id: typo
  unknown: true
processes:
  api:
    command: api
"#,
    );

    let output = keep(
        config_dir.path(),
        working_dir.path(),
        &["config", "validate", "--all"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field `unknown`"));
}

#[test]
fn config_init_creates_a_valid_minimal_global_template() {
    let workspace = TempDir::new().unwrap();
    let project = workspace.path().join("working-copy");
    let config_dir = workspace.path().join("config");
    fs::create_dir(&project).unwrap();

    let output = keep(
        &config_dir,
        &project,
        &["config", "init", "--project", "shop"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let target = config_dir.join("shop.yaml");
    let contents = fs::read_to_string(&target).unwrap();
    assert!(contents.contains("version: 1\n"), "{contents}");
    assert!(contents.contains("  name: shop\n"), "{contents}");
    assert!(!contents.contains("  id:"), "{contents}");
    assert!(
        contents.contains(&format!(
            "  path: {}\n",
            serde_json::to_string(&project.canonicalize().unwrap().to_string_lossy()).unwrap()
        )),
        "{contents}"
    );
    assert!(contents.contains("  app:\n    command:"), "{contents}");
    assert!(!contents.contains("defaults:"), "{contents}");
    assert!(!contents.contains("readiness:"), "{contents}");

    let validate = keep(
        &config_dir,
        &project,
        &["config", "validate", "--config", target.to_str().unwrap()],
    );
    assert!(
        validate.status.success(),
        "{}",
        String::from_utf8_lossy(&validate.stderr)
    );
}

#[test]
fn config_init_local_uses_git_root_remote_and_refuses_overwrite() {
    let workspace = TempDir::new().unwrap();
    let repository = workspace.path().join("shop");
    let nested = repository.join("services/api");
    let config_dir = TempDir::new().unwrap();
    fs::create_dir_all(&nested).unwrap();
    assert!(Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "https://developer:secret@github.com/acme/shop.git",
        ])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());

    let output = keep(config_dir.path(), &nested, &["config", "init", "--local"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let target = repository.join("keep.yaml");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("then run `keep start`"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("--config"));
    let contents = fs::read_to_string(&target).unwrap();
    assert!(contents.contains("  name: shop\n"), "{contents}");
    assert!(!contents.contains("  id:"), "{contents}");
    assert!(
        contents.contains("    - \"github.com/acme/shop\"\n"),
        "{contents}"
    );
    assert!(!contents.contains("secret"), "{contents}");
    assert!(!contents.contains("  path:"), "{contents}");
    assert!(!nested.join("keep.yaml").exists());

    let validate = keep(
        config_dir.path(),
        &nested,
        &["config", "validate", "--config", target.to_str().unwrap()],
    );
    assert!(validate.status.success());

    let second = keep(config_dir.path(), &nested, &["config", "init", "--local"]);
    assert!(!second.status.success());
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("configuration already exists"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(fs::read_to_string(target).unwrap(), contents);
}
