pub(crate) mod doctor;
pub(crate) mod passthrough;
pub(crate) mod workflow;

pub(crate) use self::doctor::*;
pub(crate) use self::passthrough::*;
pub(crate) use self::workflow::*;

use std::{ffi::OsString, path::Path};

#[cfg(test)]
use std::fs;

#[cfg(test)]
use crate::{
    config::{CONFIG_FILE_NAME, ProjectConfig},
    env_model::ResolvedEnvironment,
    state::ContainerSummary,
};

use crate::{
    backend::{BackendError, ContainerBackend, ContainerCliBackend, ContainerRuntime, ExecRequest},
    cli::{Cli, Commands},
    error::OrodruinError,
};

pub fn run<I, T>(args: I) -> Result<(), OrodruinError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    let program_name = args
        .first()
        .and_then(|value| Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("orodruin")
        .to_string();
    let runtime = runtime_for_os(std::env::consts::OS)?;
    let cli = Cli::parse_for_runtime_named(args, runtime, &program_name)
        .unwrap_or_else(|error| error.exit());
    let timeout = configured_timeout_for_cli(&cli)?;
    let backend = ContainerCliBackend::new(cli.debug, runtime, timeout);
    run_with_backend_for_runtime(cli, &backend, runtime)
}

#[cfg(test)]
fn run_with_backend(cli: Cli, backend: &dyn ContainerBackend) -> Result<(), OrodruinError> {
    run_with_backend_for_runtime(cli, backend, ContainerRuntime::AppleContainer)
}

fn run_with_backend_for_runtime(
    cli: Cli,
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
) -> Result<(), OrodruinError> {
    let prompt = prompt_strategy(cli.yes);
    match cli.command {
        Commands::Init => init_command()?,
        Commands::Create(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment_for_runtime_with_prompt(backend, runtime, &resolved, prompt)?;
            println!("ready {}", resolved.container_name);
        }
        Commands::Enter(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved =
                resolve_optional_environment(&loaded, environment.env.as_deref(), "enter")?;
            materialize_environment_for_runtime_with_prompt(backend, runtime, &resolved, prompt)?;
            ensure_container_user(backend, &resolved)?;
            let extra_env = parse_extra_env(&environment.env_vars)?;
            match backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved, &extra_env),
                    command: resolved.shell.clone(),
                    interactive: true,
                    user: Some(resolved.user.clone()),
                },
            ) {
                Ok(()) => {}
                Err(BackendError::CommandFailed { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }

        Commands::Run(run) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_optional_environment(&loaded, run.env.as_deref(), "run")?;
            materialize_environment_for_runtime_with_prompt(backend, runtime, &resolved, prompt)?;
            let extra_env = parse_extra_env(&run.env_vars)?;
            let command = resolve_run_command(&resolved, run)?;
            ensure_container_user(backend, &resolved)?;
            backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved, &extra_env),
                    command,
                    interactive: false,
                    user: Some(resolved.user.clone()),
                },
            )?;
        }
        Commands::List(command) => {
            let loaded = load_config(cli.config.as_deref())?;
            if command.json {
                print_json(&list_environment_entries_with_prompt(
                    backend, runtime, &loaded, prompt,
                )?)?;
            } else {
                list_environments_with_prompt(backend, runtime, &loaded, prompt)?;
            }
        }
        Commands::Rm(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            if !container_exists_for_runtime_with_prompt(
                backend,
                runtime,
                &resolved.container_name,
                prompt,
            )? {
                return Err(OrodruinError::Message(format!(
                    "environment `{}` is not created",
                    resolved.environment_name
                )));
            }
            backend.delete(&resolved.container_name)?;
            println!("removed {}", resolved.container_name);
        }
        Commands::Inspect(command) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment_by_name(&loaded, &command.env)?;
            let report = inspect_environment_with_prompt(backend, runtime, &resolved, prompt)?;
            if command.json {
                print_json(&report)?;
            } else {
                print_inspect_report(&report);
            }
        }
        Commands::Doctor(command) => {
            doctor_command(backend, runtime, cli.config.as_deref(), command)?
        }
        Commands::Pull(args) => {
            run_passthrough(backend, runtime, pull_passthrough(runtime, args), prompt)?
        }
        Commands::Images(args) => {
            run_passthrough(backend, runtime, images_passthrough(runtime, args), prompt)?
        }
        Commands::Rmi(args) => {
            run_passthrough(backend, runtime, rmi_passthrough(runtime, args), prompt)?
        }
        Commands::Ps(args) => {
            run_passthrough(backend, runtime, ps_passthrough(runtime, args), prompt)?
        }
        Commands::Logs(args) => {
            run_passthrough(backend, runtime, logs_passthrough(runtime, args), prompt)?
        }
        Commands::Build(args) => {
            run_passthrough(backend, runtime, build_passthrough(runtime, args), prompt)?
        }
        Commands::Copy(args) => {
            run_passthrough(backend, runtime, copy_passthrough(runtime, args), prompt)?
        }
        Commands::Login(args) => {
            run_passthrough(backend, runtime, login_passthrough(runtime, args), prompt)?
        }
        Commands::Logout(args) => {
            run_passthrough(backend, runtime, logout_passthrough(runtime, args), prompt)?
        }
        Commands::Image(command) => run_passthrough(
            backend,
            runtime,
            image_passthrough(runtime, command)?,
            prompt,
        )?,
        Commands::Container(command) => run_passthrough(
            backend,
            runtime,
            container_passthrough(runtime, command),
            prompt,
        )?,
        Commands::Registry(command) => run_passthrough(
            backend,
            runtime,
            registry_passthrough(runtime, command)?,
            prompt,
        )?,
        Commands::Volume(command) => run_passthrough(
            backend,
            runtime,
            resource_passthrough(runtime, "volume", command),
            prompt,
        )?,
        Commands::Network(command) => run_passthrough(
            backend,
            runtime,
            resource_passthrough(runtime, "network", command),
            prompt,
        )?,
        Commands::Builder(command) => run_passthrough(
            backend,
            runtime,
            builder_passthrough(runtime, command)?,
            prompt,
        )?,
        Commands::System(command) => run_passthrough(
            backend,
            runtime,
            system_passthrough(runtime, command)?,
            prompt,
        )?,
        Commands::Machine(command) => run_passthrough(
            backend,
            runtime,
            machine_passthrough(runtime, command)?,
            prompt,
        )?,
        Commands::Completions(command) => print!("{}", render_completions(runtime, command)?),
        Commands::Version => println!("{}", crate::build_info::render()),
    }

    Ok(())
}

#[cfg(test)]
mod tests;
