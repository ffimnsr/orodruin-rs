use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use serde_json::json;

use super::*;
use crate::{
    backend::{BackendError, CommandSpec},
    cli::{
        BuilderCommands, Commands, CompletionsCommand, ContainerCommands, EnterCommand,
        EnvironmentName, ImageCommands, InspectCommand, ListCommand, MachineCommands,
        OptionalPassthroughArgs, RegistryCommands, RequiredPassthroughArgs, ResourceCommands,
        RunCommand, SystemCommands,
    },
    config::{EnvironmentConfig, ProjectMetadata},
    env_model::ResolvedBuild,
};

#[derive(Default)]
struct MockBackend {
    list_results: RefCell<VecDeque<Vec<ContainerSummary>>>,
    inspect_value: RefCell<Option<serde_json::Value>>,
    inspect_calls: RefCell<Vec<String>>,
    system_running_results: RefCell<VecDeque<Result<bool, BackendError>>>,
    created: RefCell<Vec<String>>,
    started: RefCell<Vec<String>>,
    system_starts: RefCell<usize>,
    deleted: RefCell<Vec<String>>,
    execs: RefCell<Vec<ExecRequest>>,
    exec_results: RefCell<VecDeque<Result<(), BackendError>>>,
    builds: RefCell<Vec<String>>,
    commands: RefCell<Vec<(String, CommandSpec)>>,
}

impl MockBackend {
    fn with_lists(lists: Vec<Vec<ContainerSummary>>) -> Self {
        Self {
            list_results: RefCell::new(lists.into()),
            ..Self::default()
        }
    }
}

static CURRENT_DIR_LOCK: Mutex<()> = Mutex::new(());
static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

struct CurrentDirGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    previous: PathBuf,
}

impl<'a> CurrentDirGuard<'a> {
    fn enter(path: &Path) -> Self {
        let lock = CURRENT_DIR_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for CurrentDirGuard<'_> {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).unwrap();
    }
}

struct EnvVarGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    name: &'static str,
    previous: Option<OsString>,
}

impl<'a> EnvVarGuard<'a> {
    fn set(name: &'static str, value: impl Into<OsString>) -> Self {
        let lock = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = std::env::var_os(name);
        let value = value.into();
        unsafe {
            std::env::set_var(name, &value);
        }
        Self {
            _lock: lock,
            name,
            previous,
        }
    }
}

impl Drop for EnvVarGuard<'_> {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => unsafe {
                std::env::set_var(self.name, previous);
            },
            None => unsafe {
                std::env::remove_var(self.name);
            },
        }
    }
}

impl ContainerBackend for MockBackend {
    fn list_all(&self) -> Result<Vec<ContainerSummary>, BackendError> {
        Ok(self
            .list_results
            .borrow_mut()
            .pop_front()
            .unwrap_or_default())
    }

    fn inspect_raw(&self, container_name: &str) -> Result<Option<serde_json::Value>, BackendError> {
        self.inspect_calls
            .borrow_mut()
            .push(container_name.to_string());
        Ok(self.inspect_value.borrow().clone())
    }

    fn system_running(&self) -> Result<bool, BackendError> {
        self.system_running_results
            .borrow_mut()
            .pop_front()
            .unwrap_or(Ok(true))
    }

    fn start_system(&self) -> Result<(), BackendError> {
        *self.system_starts.borrow_mut() += 1;
        Ok(())
    }

    fn build_image(&self, build: &ResolvedBuild) -> Result<(), BackendError> {
        self.builds.borrow_mut().push(build.tag.clone());
        Ok(())
    }

    fn create(&self, environment: &ResolvedEnvironment) -> Result<(), BackendError> {
        self.created
            .borrow_mut()
            .push(environment.container_name.clone());
        Ok(())
    }

    fn start(&self, container_name: &str) -> Result<(), BackendError> {
        self.started.borrow_mut().push(container_name.to_string());
        Ok(())
    }

    fn exec(&self, _container_name: &str, request: &ExecRequest) -> Result<(), BackendError> {
        self.execs.borrow_mut().push(request.clone());
        self.exec_results.borrow_mut().pop_front().unwrap_or(Ok(()))
    }

    fn delete(&self, container_name: &str) -> Result<(), BackendError> {
        self.deleted.borrow_mut().push(container_name.to_string());
        Ok(())
    }

    fn run_command(&self, step: &str, spec: &CommandSpec) -> Result<(), BackendError> {
        self.commands
            .borrow_mut()
            .push((step.to_string(), spec.clone()));
        Ok(())
    }
}

fn write_config(root: &Path) {
    fs::write(
        root.join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"

            [envs.dev]
            image = "ubuntu:latest"
            shell = ["/bin/bash"]
            startup_command = ["sleep", "infinity"]

            [envs.ci]
            build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
            default_command = ["cargo", "test"]
        "#,
    )
    .unwrap();
}

fn resolved_name(root: &Path, env_name: &str) -> String {
    let loaded = ProjectConfig::load_from(root, Some(&root.join(CONFIG_FILE_NAME))).unwrap();
    let config = loaded.config.envs.get(env_name).unwrap();
    ResolvedEnvironment::resolve(&loaded.root, &loaded.config, env_name, config).container_name
}

fn write_runtime_stub(root: &Path, name: &str) {
    fs::write(root.join(name), "#!/bin/sh\nexit 0\n").unwrap();
}

#[test]
fn create_is_idempotent_for_existing_running_container() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: container_name.clone(),
        name: Some(container_name),
        status: Some("running".into()),
        running: true,
    }]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Create(EnvironmentName { env: "dev".into() }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert!(backend.created.borrow().is_empty());
    assert!(backend.started.borrow().is_empty());
}

#[test]
fn enter_starts_before_exec_when_needed() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: container_name.clone(),
        name: Some(container_name),
        status: Some("stopped".into()),
        running: false,
    }]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: Some("dev".into()),
            env_vars: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(backend.started.borrow().len(), 1);
    assert_eq!(backend.execs.borrow().len(), 2);
    assert_eq!(
        backend.execs.borrow()[1].command,
        vec![String::from("/bin/bash")]
    );
    assert_eq!(
        backend.execs.borrow()[1].user.as_ref().map(|user| user.uid),
        Some(unsafe { libc::getuid() })
    );
}

#[test]
fn enter_prompts_before_starting_container_system() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    ensure_container_system_running_with_prompt(&backend, ContainerRuntime::AppleContainer, || {
        Ok(true)
    })
    .unwrap();
    assert_eq!(*backend.system_starts.borrow(), 1);
}

#[test]
fn enter_aborts_when_container_system_prompt_declined() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    let error = ensure_container_system_running_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        || Ok(false),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("container system is not running")
    );
    assert!(error.to_string().contains("--yes"));
    assert_eq!(*backend.system_starts.borrow(), 0);
}

#[test]
fn podman_runtime_skips_container_system_prompt() {
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    ensure_container_system_running_with_prompt(&backend, ContainerRuntime::Podman, || {
        panic!("podman should not prompt for a container system startup")
    })
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 0);
}

#[test]
fn prompt_accepts_yes_and_no() {
    let mut stdin = std::io::Cursor::new(b"maybe\ny\n");
    let mut stderr = Vec::new();
    let accepted = prompt_to_start_container_system_with_io(&mut stdin, &mut stderr).unwrap();
    assert!(accepted);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("please answer y or n")
    );

    let mut stdin = std::io::Cursor::new(b"n\n");
    let mut stderr = Vec::new();
    let declined = prompt_to_start_container_system_with_io(&mut stdin, &mut stderr).unwrap();
    assert!(!declined);
}

#[test]
fn auto_yes_starts_container_system_without_prompt() {
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    ensure_container_system_running_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        prompt_strategy(true),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 1);
}

#[test]
fn list_entries_include_state_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let loaded = load_config(None).unwrap();
    let dev_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: dev_name.clone(),
        name: Some(dev_name.clone()),
        status: Some("Up 10 seconds".into()),
        running: true,
    }]]);

    let entries =
        list_environment_entries_with_prompt(&backend, ContainerRuntime::Podman, &loaded, || {
            Ok(true)
        })
        .unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].env, "ci");
    assert!(!entries[0].exists);
    assert_eq!(entries[0].state, "not-created");
    assert_eq!(entries[1].env, "dev");
    assert!(entries[1].exists);
    assert!(entries[1].running);
    assert_eq!(entries[1].container, dev_name);
    assert_eq!(entries[1].state, "running");
}

#[test]
fn list_starts_container_system_before_listing() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let loaded = load_config(None).unwrap();
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::with_lists(vec![vec![]])
    };

    list_environments_with_prompt(&backend, ContainerRuntime::AppleContainer, &loaded, || {
        Ok(true)
    })
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 1);
}

#[test]
fn materialize_environment_starts_container_system_before_listing() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let loaded = load_config(None).unwrap();
    let resolved = resolve_environment(&loaded, &EnvironmentName { env: "dev".into() }).unwrap();

    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::with_lists(vec![vec![]])
    };

    materialize_environment_for_runtime_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        &resolved,
        || Ok(true),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 1);
}

#[test]
fn container_exists_starts_container_system_before_lookup() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::with_lists(vec![vec![]])
    };

    container_exists_for_runtime_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        &container_name,
        || Ok(true),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 1);
}

#[test]
fn passthrough_starts_container_system_for_apple_runtime() {
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    run_passthrough_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        ps_passthrough(
            ContainerRuntime::AppleContainer,
            OptionalPassthroughArgs { args: vec![] },
        ),
        || Ok(true),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 1);
    assert_eq!(backend.commands.borrow().len(), 1);
}

#[test]
fn system_status_passthrough_skips_container_system_startup() {
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    run_passthrough_with_prompt(
        &backend,
        ContainerRuntime::AppleContainer,
        system_passthrough(
            ContainerRuntime::AppleContainer,
            SystemCommands::Status(OptionalPassthroughArgs { args: vec![] }),
        )
        .unwrap(),
        || panic!("system status should not prompt for container system startup"),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 0);
    assert_eq!(backend.commands.borrow().len(), 1);
}

#[test]
fn podman_passthrough_skips_container_system_startup() {
    let backend = MockBackend {
        system_running_results: RefCell::new(VecDeque::from([Ok(false)])),
        ..MockBackend::default()
    };

    run_passthrough_with_prompt(
        &backend,
        ContainerRuntime::Podman,
        ps_passthrough(
            ContainerRuntime::Podman,
            OptionalPassthroughArgs { args: vec![] },
        ),
        || panic!("podman should not prompt for container system startup"),
    )
    .unwrap();

    assert_eq!(*backend.system_starts.borrow(), 0);
    assert_eq!(backend.commands.borrow().len(), 1);
}

#[test]
fn enter_ignores_interactive_shell_exit_status() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);
    *backend.exec_results.borrow_mut() = VecDeque::from([
        Ok(()),
        Err(BackendError::CommandFailed {
            step: "exec command".into(),
            command: "container exec demo /bin/bash".into(),
            status: Some(127),
            detail: "the container runtime exited unsuccessfully".into(),
        }),
    ]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: Some("dev".into()),
            env_vars: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(
        backend.execs.borrow()[1].command,
        vec![String::from("/bin/bash")]
    );
}

#[test]
fn rm_targets_resolved_container() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: container_name.clone(),
        name: Some(container_name),
        status: Some("running".into()),
        running: true,
    }]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Rm(EnvironmentName { env: "dev".into() }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(backend.deleted.borrow().len(), 1);
}

#[test]
fn run_uses_default_command_when_not_provided() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Run(RunCommand {
            env: Some("ci".into()),
            env_vars: vec![],
            command: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(
        backend.execs.borrow()[1].command,
        vec![String::from("cargo"), String::from("test")]
    );
    assert_eq!(backend.builds.borrow()[0], "demo-ci:dev");
}

#[test]
fn run_uses_default_environment_when_not_provided() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"
            default_env = "ci"

            [envs.dev]
            image = "ubuntu:latest"
            shell = ["/bin/bash"]
            startup_command = ["sleep", "infinity"]

            [envs.ci]
            build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
            default_command = ["cargo", "test"]
        "#,
    )
    .unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Run(RunCommand {
            env: None,
            env_vars: vec![],
            command: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(
        backend.execs.borrow()[1].command,
        vec![String::from("cargo"), String::from("test")]
    );
    assert_eq!(backend.builds.borrow()[0], "demo-ci:dev");
}

#[test]
fn configured_timeout_prefers_cli_then_env_then_project() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"
            default_env = "dev"
            default_timeout = "45s"

            [envs.dev]
            image = "ubuntu:latest"
            timeout = "5s"

            [envs.ci]
            image = "ubuntu:latest"
        "#,
    )
    .unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let env_cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Create(EnvironmentName { env: "dev".into() }),
    };
    assert_eq!(
        configured_timeout_for_cli(&env_cli).unwrap(),
        Some(Duration::from_secs(5))
    );

    let project_cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::List(ListCommand { json: false }),
    };
    assert_eq!(
        configured_timeout_for_cli(&project_cli).unwrap(),
        Some(Duration::from_secs(45))
    );

    let override_cli = Cli {
        debug: false,
        yes: false,
        timeout: Some(Duration::from_secs(2)),
        config: None,
        command: Commands::Enter(EnterCommand {
            env: None,
            env_vars: vec![],
        }),
    };
    assert_eq!(
        configured_timeout_for_cli(&override_cli).unwrap(),
        Some(Duration::from_secs(2))
    );
}

#[test]
fn passthrough_uses_project_timeout_when_config_present() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"
            default_timeout = "12s"

            [envs.dev]
            image = "ubuntu:latest"
        "#,
    )
    .unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Pull(RequiredPassthroughArgs {
            args: vec!["alpine:latest".into()],
        }),
    };
    assert_eq!(
        configured_timeout_for_cli(&cli).unwrap(),
        Some(Duration::from_secs(12))
    );
}

#[test]
fn passthrough_skips_timeout_when_config_missing() {
    let tempdir = tempfile::tempdir().unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Ps(OptionalPassthroughArgs { args: vec![] }),
    };
    assert_eq!(configured_timeout_for_cli(&cli).unwrap(), None);
}

#[test]
fn doctor_environment_reports_missing_inputs() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"

            [envs.dev]
            image = "ubuntu:latest"
            preserve_env = ["MISSING_DOCTOR_ENV"]

            [envs.ci]
            build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
        "#,
    )
    .unwrap();

    let loaded =
        ProjectConfig::load_from(tempdir.path(), Some(&tempdir.path().join(CONFIG_FILE_NAME)))
            .unwrap();
    let dev = doctor_environment_report(
        &loaded.root,
        &loaded.config,
        "dev",
        loaded.config.envs.get("dev").unwrap(),
    );
    let ci = doctor_environment_report(
        &loaded.root,
        &loaded.config,
        "ci",
        loaded.config.envs.get("ci").unwrap(),
    );

    assert!(!dev.healthy);
    assert!(
        dev.checks
            .iter()
            .any(|check| check.name == "preserve env MISSING_DOCTOR_ENV" && !check.ok)
    );
    assert!(!ci.healthy);
    assert!(
        ci.checks
            .iter()
            .any(|check| check.name == "build file" && !check.ok)
    );
}

#[test]
fn doctor_report_marks_missing_runtime_binary() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    fs::write(tempdir.path().join("Containerfile"), "FROM ubuntu:latest\n").unwrap();
    let _cwd = CurrentDirGuard::enter(tempdir.path());
    let _path = EnvVarGuard::set("PATH", tempdir.path().join("bin-missing"));

    let backend = MockBackend::default();
    let report = doctor_report(&backend, ContainerRuntime::Podman, None).unwrap();

    assert!(!report.healthy);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "runtime binary" && !check.ok)
    );
}

#[test]
fn doctor_report_is_healthy_when_requirements_exist() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"

            [envs.dev]
            image = "ubuntu:latest"

            [envs.ci]
            build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
        "#,
    )
    .unwrap();
    fs::write(tempdir.path().join("Containerfile"), "FROM ubuntu:latest\n").unwrap();
    write_runtime_stub(tempdir.path(), "podman");
    let _cwd = CurrentDirGuard::enter(tempdir.path());
    let _path = EnvVarGuard::set("PATH", tempdir.path());

    let backend = MockBackend::default();
    let report = doctor_report(&backend, ContainerRuntime::Podman, None).unwrap();

    assert!(report.healthy);
    assert!(report.checks.iter().all(|check| check.ok));
    assert!(
        report
            .environments
            .iter()
            .all(|environment| environment.healthy)
    );
}

#[test]
fn inspect_reports_backend_payload() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let container_name = resolved_name(tempdir.path(), "dev");
    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: container_name.clone(),
        name: Some(container_name.clone()),
        status: Some("running".into()),
        running: true,
    }]]);
    *backend.inspect_value.borrow_mut() = Some(json!({ "name": "demo" }));

    let loaded = load_config(None).unwrap();
    let resolved = resolve_environment(&loaded, &EnvironmentName { env: "dev".into() }).unwrap();
    let payload = backend
        .inspect_raw(&resolved.container_name)
        .unwrap()
        .unwrap();
    assert_eq!(backend.inspect_calls.borrow().as_slice(), &[container_name]);
    assert_eq!(payload["name"], "demo");
}

#[test]
fn inspect_skips_backend_lookup_for_missing_container() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Inspect(InspectCommand {
            env: "dev".into(),
            json: false,
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert!(backend.inspect_calls.borrow().is_empty());
}

#[test]
fn existing_built_container_does_not_rebuild_image() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "ci");

    let backend = MockBackend::with_lists(vec![vec![ContainerSummary {
        id: container_name.clone(),
        name: Some(container_name),
        status: Some("running".into()),
        running: true,
    }]]);

    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Create(EnvironmentName { env: "ci".into() }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert!(backend.builds.borrow().is_empty());
}

#[test]
fn project_resolution_uses_config_name() {
    let project = ProjectConfig {
        project: ProjectMetadata {
            name: Some("Demo".into()),
            default_env: None,
            default_timeout: None,
        },
        envs: Default::default(),
    };
    let env = EnvironmentConfig {
        image: Some("ubuntu:latest".into()),
        build: None,
        container_name: None,
        project_mount: None,
        workdir: None,
        timeout: None,
        env: Default::default(),
        preserve_env: vec![],
        mounts: vec![],
        env_files: vec![],
        shell: None,
        startup_command: None,
        default_command: None,
    };

    let resolved = ResolvedEnvironment::resolve(Path::new("/tmp/demo"), &project, "dev", &env);
    assert!(resolved.container_name.starts_with("orodruin-demo-"));
}

#[test]
fn enter_bootstraps_user_before_shell_exec() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: Some("dev".into()),
            env_vars: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();

    assert_eq!(backend.execs.borrow().len(), 2);
    let execs = backend.execs.borrow();
    assert_eq!(execs[0].command[0], "/bin/sh");
    assert!(execs[0].command[2].contains("useradd -K UID_MIN=0 -K UID_MAX=60000"));
    assert!(
        execs[0].command[2]
            .contains("useradd -m -d \"$home\" -u \"$uid\" -g \"$gid\" -s /bin/sh \"$username\"")
    );
    assert_eq!(execs[0].user.as_ref().map(|user| user.uid), Some(0));
    assert_eq!(execs[0].user.as_ref().map(|user| user.gid), Some(0));
    assert_eq!(
        execs[1].env.iter().find(|(key, _)| key == "HOME"),
        Some(&(
            String::from("HOME"),
            format!("/home/{}", execs[1].user.as_ref().unwrap().username)
        ))
    );
}

#[test]
fn bootstrap_user_script_reuses_existing_uid_owner() {
    let script = bootstrap_user_script();
    assert!(script.contains("username=\"$uid_owner\""));
    assert!(script.contains(
        "uid ${uid} already owned by ${uid_owner} with gid ${existing_gid}, expected ${gid}"
    ));
}

#[test]
fn pull_dispatches_to_image_pull() {
    let backend = MockBackend::default();
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Pull(RequiredPassthroughArgs {
            args: vec!["alpine:latest".into()],
        }),
    };

    run_with_backend(cli, &backend).unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].0, "pull image");
    assert_eq!(commands[0].1.args, ["image", "pull", "alpine:latest"]);
}

#[test]
fn ps_dispatches_to_container_list() {
    let backend = MockBackend::default();
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Ps(OptionalPassthroughArgs {
            args: vec!["--all".into()],
        }),
    };

    run_with_backend(cli, &backend).unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].0, "list containers");
    assert_eq!(commands[0].1.args, ["list", "--all"]);
}

#[test]
fn nested_registry_and_resource_commands_map_to_container_cli() {
    let backend = MockBackend::default();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Registry(RegistryCommands::List(OptionalPassthroughArgs {
                args: vec![],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Prune(OptionalPassthroughArgs {
                args: vec![],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Volume(ResourceCommands::Remove(RequiredPassthroughArgs {
                args: vec!["cache".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["registry", "list"]);
    assert_eq!(commands[1].1.args, ["image", "prune"]);
    assert_eq!(commands[2].1.args, ["volume", "delete", "cache"]);
}

#[test]
fn nested_image_commands_map_to_container_cli() {
    let backend = MockBackend::default();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Inspect(RequiredPassthroughArgs {
                args: vec!["alpine:latest".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Push(RequiredPassthroughArgs {
                args: vec!["demo:latest".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Tag(RequiredPassthroughArgs {
                args: vec!["demo:latest".into(), "demo:v1".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["image", "inspect", "alpine:latest"]);
    assert_eq!(commands[1].1.args, ["image", "push", "demo:latest"]);
    assert_eq!(
        commands[2].1.args,
        ["image", "tag", "demo:latest", "demo:v1"]
    );
}

#[test]
fn image_load_and_save_map_to_container_cli() {
    let backend = MockBackend::default();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Load(OptionalPassthroughArgs {
                args: vec!["-i".into(), "image.tar".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Image(ImageCommands::Save(OptionalPassthroughArgs {
                args: vec!["-o".into(), "image.tar".into(), "demo:latest".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["image", "load", "-i", "image.tar"]);
    assert_eq!(
        commands[1].1.args,
        ["image", "save", "-o", "image.tar", "demo:latest"]
    );
}

#[test]
fn nested_container_commands_map_to_container_cli() {
    let backend = MockBackend::default();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Container(ContainerCommands::Inspect(RequiredPassthroughArgs {
                args: vec!["demo".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Container(ContainerCommands::Start(RequiredPassthroughArgs {
                args: vec!["demo".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Container(ContainerCommands::Stop(RequiredPassthroughArgs {
                args: vec!["demo".into()],
            })),
        },
        &backend,
    )
    .unwrap();

    run_with_backend(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Container(ContainerCommands::Prune(OptionalPassthroughArgs {
                args: vec![],
            })),
        },
        &backend,
    )
    .unwrap();

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["inspect", "demo"]);
    assert_eq!(commands[1].1.args, ["start", "demo"]);
    assert_eq!(commands[2].1.args, ["stop", "demo"]);
    assert_eq!(commands[3].1.args, ["prune"]);
}

#[test]
fn extended_container_commands_map_to_container_cli() {
    let backend = MockBackend::default();

    for command in [
        Commands::Container(ContainerCommands::Create(OptionalPassthroughArgs {
            args: vec!["--name".into(), "demo".into(), "alpine".into()],
        })),
        Commands::Container(ContainerCommands::Run(OptionalPassthroughArgs {
            args: vec!["--rm".into(), "alpine".into(), "echo".into(), "hi".into()],
        })),
        Commands::Container(ContainerCommands::Exec(OptionalPassthroughArgs {
            args: vec!["demo".into(), "sh".into()],
        })),
        Commands::Container(ContainerCommands::Kill(OptionalPassthroughArgs {
            args: vec!["demo".into()],
        })),
        Commands::Container(ContainerCommands::Stats(OptionalPassthroughArgs {
            args: vec!["demo".into()],
        })),
        Commands::Container(ContainerCommands::Export(OptionalPassthroughArgs {
            args: vec!["demo".into()],
        })),
    ] {
        run_with_backend(
            Cli {
                debug: false,
                yes: false,
                timeout: None,
                config: None,
                command,
            },
            &backend,
        )
        .unwrap();
    }

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["create", "--name", "demo", "alpine"]);
    assert_eq!(commands[1].1.args, ["run", "--rm", "alpine", "echo", "hi"]);
    assert_eq!(commands[2].1.args, ["exec", "demo", "sh"]);
    assert_eq!(commands[3].1.args, ["kill", "demo"]);
    assert_eq!(commands[4].1.args, ["stats", "demo"]);
    assert_eq!(commands[5].1.args, ["export", "demo"]);
}

#[test]
fn builder_system_and_machine_commands_map_to_container_cli() {
    let backend = MockBackend::default();

    for command in [
        Commands::Builder(BuilderCommands::Status(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::System(SystemCommands::Version(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::Machine(MachineCommands::List(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::Machine(MachineCommands::SetDefault(OptionalPassthroughArgs {
            args: vec!["desktop".into()],
        })),
    ] {
        run_with_backend(
            Cli {
                debug: false,
                yes: false,
                timeout: None,
                config: None,
                command,
            },
            &backend,
        )
        .unwrap();
    }

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.args, ["builder", "status"]);
    assert_eq!(commands[1].1.args, ["system", "version"]);
    assert_eq!(commands[2].1.args, ["machine", "list"]);
    assert_eq!(commands[3].1.args, ["machine", "set-default", "desktop"]);
}

#[test]
fn podman_passthrough_commands_use_linux_cli_shape() {
    let backend = MockBackend::default();

    for command in [
        Commands::Pull(RequiredPassthroughArgs {
            args: vec!["alpine:latest".into()],
        }),
        Commands::Ps(OptionalPassthroughArgs {
            args: vec!["--all".into()],
        }),
        Commands::Copy(RequiredPassthroughArgs {
            args: vec!["host.txt".into(), "demo:/tmp/host.txt".into()],
        }),
        Commands::Volume(ResourceCommands::List(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::Network(ResourceCommands::Remove(RequiredPassthroughArgs {
            args: vec!["demo-net".into()],
        })),
        Commands::Container(ContainerCommands::Remove(RequiredPassthroughArgs {
            args: vec!["demo".into()],
        })),
        Commands::System(SystemCommands::Version(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::System(SystemCommands::Status(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::Machine(MachineCommands::Create(OptionalPassthroughArgs {
            args: vec!["devvm".into()],
        })),
        Commands::Machine(MachineCommands::Run(OptionalPassthroughArgs {
            args: vec!["devvm".into()],
        })),
        Commands::Builder(BuilderCommands::Status(OptionalPassthroughArgs {
            args: vec![],
        })),
        Commands::Builder(BuilderCommands::Remove(OptionalPassthroughArgs {
            args: vec!["--all".into()],
        })),
    ] {
        run_with_backend_for_runtime(
            Cli {
                debug: false,
                yes: false,
                timeout: None,
                config: None,
                command,
            },
            &backend,
            ContainerRuntime::Podman,
        )
        .unwrap();
    }

    let commands = backend.commands.borrow();
    assert_eq!(commands[0].1.program, "podman");
    assert_eq!(commands[0].1.args, ["pull", "alpine:latest"]);
    assert_eq!(commands[1].1.args, ["ps", "--all"]);
    assert_eq!(commands[2].1.args, ["cp", "host.txt", "demo:/tmp/host.txt"]);
    assert_eq!(commands[3].1.args, ["volume", "ls"]);
    assert_eq!(commands[4].1.args, ["network", "rm", "demo-net"]);
    assert_eq!(commands[5].1.args, ["container", "rm", "demo"]);
    assert_eq!(commands[6].1.args, ["version"]);
    assert_eq!(commands[7].1.args, ["system", "info"]);
    assert_eq!(commands[8].1.args, ["machine", "init", "devvm"]);
    assert_eq!(commands[9].1.args, ["machine", "ssh", "devvm"]);
    assert_eq!(commands[10].1.args, ["builder", "inspect"]);
    assert_eq!(commands[11].1.args, ["builder", "prune", "--all"]);
}

#[test]
fn podman_rejects_unsupported_apple_only_passthroughs() {
    let backend = MockBackend::default();
    let system_error = run_with_backend_for_runtime(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::System(SystemCommands::Dns(OptionalPassthroughArgs {
                args: vec![],
            })),
        },
        &backend,
        ContainerRuntime::Podman,
    )
    .unwrap_err();

    let registry_error = run_with_backend_for_runtime(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Registry(RegistryCommands::List(OptionalPassthroughArgs {
                args: vec![],
            })),
        },
        &backend,
        ContainerRuntime::Podman,
    )
    .unwrap_err();

    let machine_error = run_with_backend_for_runtime(
        Cli {
            debug: false,
            yes: false,
            timeout: None,
            config: None,
            command: Commands::Machine(MachineCommands::SetDefault(OptionalPassthroughArgs {
                args: vec!["devvm".into()],
            })),
        },
        &backend,
        ContainerRuntime::Podman,
    )
    .unwrap_err();

    assert!(system_error.to_string().contains("system dns"));
    assert!(registry_error.to_string().contains("registry list"));
    assert!(machine_error.to_string().contains("machine set-default"));
}

#[test]
fn enter_uses_project_default_env_when_omitted() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"
            default_env = "ci"

            [envs.dev]
            image = "ubuntu:latest"

            [envs.ci]
            build = { context = ".", file = "Containerfile", tag = "demo-ci:dev" }
        "#,
    )
    .unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::with_lists(vec![vec![]]);
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: None,
            env_vars: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(backend.builds.borrow().as_slice(), ["demo-ci:dev"]);
}

#[test]
fn enter_uses_single_environment_when_omitted() {
    let tempdir = tempfile::tempdir().unwrap();
    fs::write(
        tempdir.path().join(CONFIG_FILE_NAME),
        r#"
            [project]
            name = "demo"

            [envs.dev]
            image = "ubuntu:latest"
            shell = ["/bin/bash"]
        "#,
    )
    .unwrap();
    let _guard = CurrentDirGuard::enter(tempdir.path());
    let container_name = resolved_name(tempdir.path(), "dev");

    let backend = MockBackend::with_lists(vec![vec![]]);
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: None,
            env_vars: vec![],
        }),
    };

    run_with_backend(cli, &backend).unwrap();
    assert_eq!(backend.created.borrow().as_slice(), [container_name]);
}

#[test]
fn enter_without_env_errors_when_ambiguous() {
    let tempdir = tempfile::tempdir().unwrap();
    write_config(tempdir.path());
    let _guard = CurrentDirGuard::enter(tempdir.path());

    let backend = MockBackend::default();
    let cli = Cli {
        debug: false,
        yes: false,
        timeout: None,
        config: None,
        command: Commands::Enter(EnterCommand {
            env: None,
            env_vars: vec![],
        }),
    };

    let error = run_with_backend(cli, &backend).unwrap_err();
    assert!(error.to_string().contains("set project.default_env"));
}

#[test]
fn completions_render_current_subcommands() {
    let output = render_completions(
        ContainerRuntime::AppleContainer,
        CompletionsCommand {
            shell: clap_complete::Shell::Bash,
        },
    )
    .unwrap();

    assert!(output.contains("completions"));
    assert!(output.contains("orodruin"));
    assert!(output.contains("inspect"));
    assert!(!output.contains("ssh"));
}

#[test]
fn podman_completions_omit_pruned_subcommands() {
    let output = render_completions(
        ContainerRuntime::Podman,
        CompletionsCommand {
            shell: clap_complete::Shell::Bash,
        },
    )
    .unwrap();

    assert!(output.contains("version"));
    assert!(!output.contains("set-default"));
    assert!(!output.contains(" dns "));
}

#[test]
fn parse_extra_env_parses_key_value_pairs() {
    let result = parse_extra_env(&["FOO=bar".into(), "X=1".into()]).unwrap();
    assert_eq!(
        result,
        vec![("FOO".into(), "bar".into()), ("X".into(), "1".into())]
    );
}

#[test]
fn parse_extra_env_rejects_missing_equals() {
    let error = parse_extra_env(&["INVALID".into()]).unwrap_err();
    assert!(error.to_string().contains("expected KEY=VALUE"));
}

#[test]
fn parse_extra_env_rejects_empty_key() {
    let error = parse_extra_env(&["=value".into()]).unwrap_err();
    assert!(error.to_string().contains("name must not be empty"));
}

#[test]
fn parse_extra_env_allows_empty_value() {
    let result = parse_extra_env(&["EMPTY=".into()]).unwrap();
    assert_eq!(result, vec![("EMPTY".into(), "".into())]);
}
