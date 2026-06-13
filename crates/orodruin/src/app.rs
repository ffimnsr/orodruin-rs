use std::{ffi::OsString, fs, path::Path};

use clap::Parser;
use serde_json::to_string_pretty;

use crate::{
    backend::{AppleContainerBackend, ContainerBackend, ExecRequest},
    cli::{Cli, Commands, EnvironmentName, RunCommand},
    config::{CONFIG_FILE_NAME, LoadedConfig, ProjectConfig, default_init_config},
    env_model::ResolvedEnvironment,
    error::OrodruinError,
    state::ContainerSummary,
};

pub fn run<I, T>(args: I) -> Result<(), OrodruinError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    let backend = AppleContainerBackend::new(cli.debug);
    run_with_backend(cli, &backend)
}

fn run_with_backend(cli: Cli, backend: &dyn ContainerBackend) -> Result<(), OrodruinError> {
    match cli.command {
        Commands::Init => init_command()?,
        Commands::Create(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment(backend, &resolved)?;
            println!("ready {}", resolved.container_name);
        }
        Commands::Enter(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment(backend, &resolved)?;
            backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: resolved
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                    command: resolved.shell.clone(),
                    interactive: true,
                },
            )?;
        }
        Commands::Run(run) => {
            let loaded = load_config(cli.config.as_deref())?;
            let environment = EnvironmentName {
                env: run.env.clone(),
            };
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment(backend, &resolved)?;
            let command = resolve_run_command(&resolved, run)?;
            backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: resolved
                        .env
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                    command,
                    interactive: false,
                },
            )?;
        }
        Commands::List => {
            let loaded = load_config(cli.config.as_deref())?;
            let containers = backend.list_all()?;
            for (name, config) in &loaded.config.envs {
                let resolved =
                    ResolvedEnvironment::resolve(&loaded.root, &loaded.config, name, config);
                let summary = containers
                    .iter()
                    .find(|summary| summary.matches(&resolved.container_name));
                print_summary(name, &resolved.container_name, summary);
            }
        }
        Commands::Rm(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            if !container_exists(backend, &resolved.container_name)? {
                return Err(OrodruinError::Message(format!(
                    "environment `{}` is not created",
                    resolved.environment_name
                )));
            }
            backend.delete(&resolved.container_name)?;
            println!("removed {}", resolved.container_name);
        }
        Commands::Inspect(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            println!("env: {}", resolved.environment_name);
            println!("container: {}", resolved.container_name);
            println!("image: {}", resolved.image);
            println!("workdir: {}", resolved.workdir);
            if !container_exists(backend, &resolved.container_name)? {
                println!("container not created");
                return Ok(());
            }
            let inspect = backend.inspect_raw(&resolved.container_name)?;
            match inspect {
                Some(value) => println!(
                    "{}",
                    to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
                ),
                None => println!("container not created"),
            }
        }
        Commands::Version => println!("{}", crate::build_info::render()),
    }

    Ok(())
}

fn init_command() -> Result<(), OrodruinError> {
    let cwd = std::env::current_dir()?;
    let path = cwd.join(CONFIG_FILE_NAME);
    if path.exists() {
        return Err(OrodruinError::Message(format!(
            "{CONFIG_FILE_NAME} already exists at {}",
            path.display()
        )));
    }

    let project_name = cwd
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("project");
    fs::write(&path, default_init_config(project_name))?;
    println!("created {}", path.display());
    Ok(())
}

fn load_config(explicit_path: Option<&Path>) -> Result<LoadedConfig, OrodruinError> {
    let cwd = std::env::current_dir()?;
    Ok(ProjectConfig::load_from(&cwd, explicit_path)?)
}

fn resolve_environment(
    loaded: &LoadedConfig,
    environment: &EnvironmentName,
) -> Result<ResolvedEnvironment, OrodruinError> {
    let config = loaded.config.envs.get(&environment.env).ok_or_else(|| {
        OrodruinError::Message(format!(
            "environment `{}` is not defined in {}",
            environment.env,
            loaded.path.display()
        ))
    })?;

    Ok(ResolvedEnvironment::resolve(
        &loaded.root,
        &loaded.config,
        &environment.env,
        config,
    ))
}

fn resolve_run_command(
    resolved: &ResolvedEnvironment,
    run: RunCommand,
) -> Result<Vec<String>, OrodruinError> {
    if !run.command.is_empty() {
        return Ok(run.command);
    }
    resolved.default_command.clone().ok_or_else(|| {
        OrodruinError::Message(format!(
            "environment `{}` has no default command; provide one after `--`",
            resolved.environment_name
        ))
    })
}

fn materialize_environment(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<(), OrodruinError> {
    let current = backend
        .list_all()?
        .into_iter()
        .find(|summary| summary.matches(&resolved.container_name));

    match current {
        Some(summary) if summary.running => Ok(()),
        Some(_) => {
            backend.start(&resolved.container_name)?;
            Ok(())
        }
        None => {
            if let Some(build) = &resolved.build {
                backend.build_image(build)?;
            }
            backend.create(resolved)?;
            Ok(())
        }
    }
}

fn container_exists(
    backend: &dyn ContainerBackend,
    container_name: &str,
) -> Result<bool, OrodruinError> {
    Ok(backend
        .list_all()?
        .iter()
        .any(|summary| summary.matches(container_name)))
}

fn print_summary(name: &str, container_name: &str, summary: Option<&ContainerSummary>) {
    let state = match summary {
        Some(summary) if summary.running => "running",
        Some(summary) => summary.status.as_deref().unwrap_or("created"),
        None => "not-created",
    };
    println!("{name}\t{container_name}\t{state}");
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::VecDeque,
        path::{Path, PathBuf},
        sync::{Mutex, MutexGuard},
    };

    use serde_json::json;

    use super::*;
    use crate::{
        backend::BackendError,
        cli::Commands,
        config::{EnvironmentConfig, ProjectMetadata},
        env_model::ResolvedBuild,
    };

    #[derive(Default)]
    struct MockBackend {
        list_results: RefCell<VecDeque<Vec<ContainerSummary>>>,
        inspect_value: RefCell<Option<serde_json::Value>>,
        inspect_calls: RefCell<Vec<String>>,
        created: RefCell<Vec<String>>,
        started: RefCell<Vec<String>>,
        deleted: RefCell<Vec<String>>,
        execs: RefCell<Vec<Vec<String>>>,
        exec_result: RefCell<Option<Result<(), BackendError>>>,
        builds: RefCell<Vec<String>>,
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

    struct CurrentDirGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        previous: PathBuf,
    }

    impl<'a> CurrentDirGuard<'a> {
        fn enter(path: &Path) -> Self {
            let lock = CURRENT_DIR_LOCK.lock().unwrap();
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

    impl ContainerBackend for MockBackend {
        fn list_all(&self) -> Result<Vec<ContainerSummary>, BackendError> {
            Ok(self
                .list_results
                .borrow_mut()
                .pop_front()
                .unwrap_or_default())
        }

        fn inspect_raw(
            &self,
            container_name: &str,
        ) -> Result<Option<serde_json::Value>, BackendError> {
            self.inspect_calls
                .borrow_mut()
                .push(container_name.to_string());
            Ok(self.inspect_value.borrow().clone())
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
            self.execs.borrow_mut().push(request.command.clone());
            self.exec_result.borrow_mut().take().unwrap_or(Ok(()))
        }

        fn delete(&self, container_name: &str) -> Result<(), BackendError> {
            self.deleted.borrow_mut().push(container_name.to_string());
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
            config: None,
            command: Commands::Enter(EnvironmentName { env: "dev".into() }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.started.borrow().len(), 1);
        assert_eq!(backend.execs.borrow()[0], vec![String::from("/bin/bash")]);
    }

    #[test]
    fn enter_ignores_interactive_shell_exit_status() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.exec_result.borrow_mut() = Some(Err(BackendError::CommandFailed {
            step: "exec command".into(),
            command: "container exec demo /bin/bash".into(),
            status: Some(127),
            stderr: String::new(),
        }));

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnvironmentName { env: "dev".into() }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert_eq!(error.exit_code(), 127);
        assert_eq!(backend.execs.borrow()[0], vec![String::from("/bin/bash")]);
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
            config: None,
            command: Commands::Run(RunCommand {
                env: "ci".into(),
                command: vec![],
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(
            backend.execs.borrow()[0],
            vec![String::from("cargo"), String::from("test")]
        );
        assert_eq!(backend.builds.borrow()[0], "demo-ci:dev");
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
        let resolved =
            resolve_environment(&loaded, &EnvironmentName { env: "dev".into() }).unwrap();
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
            config: None,
            command: Commands::Inspect(EnvironmentName { env: "dev".into() }),
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
            },
            envs: Default::default(),
        };
        let env = EnvironmentConfig {
            image: Some("ubuntu:latest".into()),
            build: None,
            container_name: None,
            project_mount: None,
            workdir: None,
            env: Default::default(),
            preserve_env: vec![],
            mounts: vec![],
            shell: None,
            startup_command: None,
            default_command: None,
        };

        let resolved = ResolvedEnvironment::resolve(Path::new("/tmp/demo"), &project, "dev", &env);
        assert!(resolved.container_name.starts_with("orodruin-demo-"));
    }
}
