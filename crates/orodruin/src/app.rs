use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, BufRead, Write},
    path::Path,
    time::Duration,
};

use clap_complete::generate;
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    backend::{BackendError, ContainerBackend, ContainerCliBackend, ContainerRuntime, ExecRequest},
    cli::{
        BuilderCommands, Cli, Commands, CompletionsCommand, ContainerCommands, DoctorCommand,
        EnvironmentName, ImageCommands, MachineCommands, OptionalPassthroughArgs, RegistryCommands,
        RequiredPassthroughArgs, ResourceCommands, RunCommand, SystemCommands,
    },
    config::{
        CONFIG_FILE_NAME, ConfigError, LoadedConfig, ProjectConfig, default_init_config,
        parse_timeout,
    },
    env_model::{ResolvedEnvironment, ResolvedUser},
    error::OrodruinError,
    state::ContainerSummary,
};

struct PassthroughInvocation {
    step: String,
    command: Vec<String>,
    requires_container_system: bool,
}

#[derive(Debug, Serialize)]
struct ListEntry {
    env: String,
    container: String,
    state: String,
    running: bool,
    exists: bool,
}

#[derive(Debug, Serialize)]
struct InspectReport<'a> {
    env: &'a str,
    container: &'a str,
    image: &'a str,
    workdir: &'a str,
    container_exists: bool,
    resolved: &'a ResolvedEnvironment,
    inspect: Option<Value>,
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorEnvironmentReport {
    env: String,
    container: String,
    image: String,
    healthy: bool,
    checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    healthy: bool,
    runtime: String,
    runtime_program: String,
    config_path: Option<String>,
    project_root: Option<String>,
    checks: Vec<DoctorCheck>,
    environments: Vec<DoctorEnvironmentReport>,
}

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

fn run_passthrough(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    invocation: PassthroughInvocation,
    prompt: fn() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    run_passthrough_with_prompt(backend, runtime, invocation, prompt)
}

fn run_passthrough_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    invocation: PassthroughInvocation,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    if invocation.requires_container_system {
        ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    }
    let spec = ContainerCliBackend::build_passthrough_spec(runtime, invocation.command);
    backend.run_command(&invocation.step, &spec)?;
    Ok(())
}

fn passthrough(step: impl Into<String>, command: Vec<String>) -> PassthroughInvocation {
    PassthroughInvocation {
        step: step.into(),
        command,
        requires_container_system: false,
    }
}

fn passthrough_requiring_container_system(
    step: impl Into<String>,
    command: Vec<String>,
) -> PassthroughInvocation {
    PassthroughInvocation {
        step: step.into(),
        command,
        requires_container_system: true,
    }
}

fn passthrough_with_args(
    step: impl Into<String>,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    let mut command = prefix.into_iter().map(str::to_string).collect::<Vec<_>>();
    command.extend(args.into());
    passthrough(step, command)
}

fn passthrough_with_args_requiring_container_system(
    step: impl Into<String>,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    let mut command = prefix.into_iter().map(str::to_string).collect::<Vec<_>>();
    command.extend(args.into());
    passthrough_requiring_container_system(step, command)
}

fn runtime_for_os(os: &str) -> Result<ContainerRuntime, OrodruinError> {
    ContainerRuntime::from_os(os).ok_or_else(|| {
        OrodruinError::Message(format!(
            "unsupported operating system `{os}`; supported platforms are macOS and Linux"
        ))
    })
}

fn render_completions(
    runtime: ContainerRuntime,
    command: CompletionsCommand,
) -> Result<String, OrodruinError> {
    let mut cli = Cli::command_for_runtime(runtime);
    let mut output = Vec::new();
    let name = cli.get_name().to_string();
    generate(command.shell, &mut cli, name, &mut output);
    String::from_utf8(output).map_err(|error| OrodruinError::Message(error.to_string()))
}

fn resource_passthrough(
    runtime: ContainerRuntime,
    resource: &str,
    command: ResourceCommands,
) -> PassthroughInvocation {
    let list = match (runtime, resource) {
        (ContainerRuntime::AppleContainer, _) => "list",
        (ContainerRuntime::Podman, "volume") => "ls",
        (ContainerRuntime::Podman, "network") => "ls",
        (ContainerRuntime::Podman, _) => "list",
    };
    let remove = match runtime {
        ContainerRuntime::AppleContainer => "delete",
        ContainerRuntime::Podman => "rm",
    };

    match command {
        ResourceCommands::List(args) => match runtime {
            ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
                format!("list {resource}s"),
                vec![resource, list],
                args,
            ),
            ContainerRuntime::Podman => {
                passthrough_owned(format!("list {resource}s"), vec![resource, list], args)
            }
        },
        ResourceCommands::Create(args) => match runtime {
            ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
                format!("create {resource}"),
                vec![resource, "create"],
                args,
            ),
            ContainerRuntime::Podman => {
                passthrough_owned(format!("create {resource}"), vec![resource, "create"], args)
            }
        },
        ResourceCommands::Inspect(args) => match runtime {
            ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
                format!("inspect {resource}"),
                vec![resource, "inspect"],
                args,
            ),
            ContainerRuntime::Podman => passthrough_owned(
                format!("inspect {resource}"),
                vec![resource, "inspect"],
                args,
            ),
        },
        ResourceCommands::Prune(args) => match runtime {
            ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
                format!("prune {resource}s"),
                vec![resource, "prune"],
                args,
            ),
            ContainerRuntime::Podman => {
                passthrough_owned(format!("prune {resource}s"), vec![resource, "prune"], args)
            }
        },
        ResourceCommands::Remove(args) => match runtime {
            ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
                format!("remove {resource}"),
                vec![resource, remove],
                args,
            ),
            ContainerRuntime::Podman => {
                passthrough_owned(format!("remove {resource}"), vec![resource, remove], args)
            }
        },
    }
}

fn passthrough_owned(
    step: String,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    passthrough_with_args(step, prefix, args)
}

fn pull_passthrough(
    runtime: ContainerRuntime,
    args: RequiredPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "pull image",
            vec!["image", "pull"],
            args,
        ),
        ContainerRuntime::Podman => passthrough_with_args("pull image", vec!["pull"], args),
    }
}

fn images_passthrough(
    runtime: ContainerRuntime,
    args: OptionalPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "list images",
            vec!["image", "list"],
            args,
        ),
        ContainerRuntime::Podman => passthrough_with_args("list images", vec!["images"], args),
    }
}

fn rmi_passthrough(
    runtime: ContainerRuntime,
    args: RequiredPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "remove image",
            vec!["image", "delete"],
            args,
        ),
        ContainerRuntime::Podman => passthrough_with_args("remove image", vec!["rmi"], args),
    }
}

fn ps_passthrough(
    runtime: ContainerRuntime,
    args: OptionalPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => {
            passthrough_with_args_requiring_container_system("list containers", vec!["list"], args)
        }
        ContainerRuntime::Podman => passthrough_with_args("list containers", vec!["ps"], args),
    }
}

fn logs_passthrough(
    runtime: ContainerRuntime,
    args: OptionalPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "show container logs",
            vec!["logs"],
            args,
        ),
        ContainerRuntime::Podman => {
            passthrough_with_args("show container logs", vec!["logs"], args)
        }
    }
}

fn build_passthrough(
    runtime: ContainerRuntime,
    args: RequiredPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => {
            passthrough_with_args_requiring_container_system("build image", vec!["build"], args)
        }
        ContainerRuntime::Podman => passthrough_with_args("build image", vec!["build"], args),
    }
}

fn copy_passthrough(
    runtime: ContainerRuntime,
    args: RequiredPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => {
            passthrough_with_args_requiring_container_system("copy files", vec!["copy"], args)
        }
        ContainerRuntime::Podman => passthrough_with_args("copy files", vec!["cp"], args),
    }
}

fn login_passthrough(
    runtime: ContainerRuntime,
    args: OptionalPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "login registry",
            vec!["registry", "login"],
            args,
        ),
        ContainerRuntime::Podman => passthrough_with_args("login registry", vec!["login"], args),
    }
}

fn logout_passthrough(
    runtime: ContainerRuntime,
    args: OptionalPassthroughArgs,
) -> PassthroughInvocation {
    match runtime {
        ContainerRuntime::AppleContainer => passthrough_with_args_requiring_container_system(
            "logout registry",
            vec!["registry", "logout"],
            args,
        ),
        ContainerRuntime::Podman => passthrough_with_args("logout registry", vec!["logout"], args),
    }
}

fn image_passthrough(
    runtime: ContainerRuntime,
    command: ImageCommands,
) -> Result<PassthroughInvocation, OrodruinError> {
    Ok(match (runtime, command) {
        (ContainerRuntime::AppleContainer, ImageCommands::Pull(args)) => {
            passthrough_with_args_requiring_container_system(
                "pull image",
                vec!["image", "pull"],
                args,
            )
        }
        (ContainerRuntime::Podman, ImageCommands::Pull(args)) => {
            passthrough_with_args("pull image", vec!["image", "pull"], args)
        }
        (ContainerRuntime::AppleContainer, ImageCommands::List(args)) => {
            passthrough_with_args_requiring_container_system(
                "list images",
                vec!["image", "list"],
                args,
            )
        }
        (ContainerRuntime::Podman, ImageCommands::List(args)) => {
            passthrough_with_args("list images", vec!["image", "list"], args)
        }
        (_, ImageCommands::Inspect(args)) => {
            passthrough_with_args("inspect image", vec!["image", "inspect"], args)
        }
        (_, ImageCommands::Load(args)) => {
            passthrough_with_args("load image", vec!["image", "load"], args)
        }
        (ContainerRuntime::AppleContainer, ImageCommands::Remove(args)) => {
            passthrough_with_args_requiring_container_system(
                "remove image",
                vec!["image", "delete"],
                args,
            )
        }
        (ContainerRuntime::Podman, ImageCommands::Remove(args)) => {
            passthrough_with_args("remove image", vec!["image", "rm"], args)
        }
        (_, ImageCommands::Push(args)) => {
            passthrough_with_args("push image", vec!["image", "push"], args)
        }
        (_, ImageCommands::Prune(args)) => {
            passthrough_with_args("prune images", vec!["image", "prune"], args)
        }
        (_, ImageCommands::Save(args)) => {
            passthrough_with_args("save image", vec!["image", "save"], args)
        }
        (_, ImageCommands::Tag(args)) => {
            passthrough_with_args("tag image", vec!["image", "tag"], args)
        }
    })
}

fn container_passthrough(
    runtime: ContainerRuntime,
    command: ContainerCommands,
) -> PassthroughInvocation {
    match (runtime, command) {
        (ContainerRuntime::AppleContainer, ContainerCommands::Create(args)) => {
            passthrough_with_args_requiring_container_system(
                "create container",
                vec!["create"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Create(args)) => {
            passthrough_with_args("create container", vec!["container", "create"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Exec(args)) => {
            passthrough_with_args_requiring_container_system(
                "exec container command",
                vec!["exec"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Exec(args)) => {
            passthrough_with_args("exec container command", vec!["container", "exec"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Export(args)) => {
            passthrough_with_args_requiring_container_system(
                "export container",
                vec!["export"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Export(args)) => {
            passthrough_with_args("export container", vec!["container", "export"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Kill(args)) => {
            passthrough_with_args_requiring_container_system("kill container", vec!["kill"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::Kill(args)) => {
            passthrough_with_args("kill container", vec!["container", "kill"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::List(args)) => {
            passthrough_with_args_requiring_container_system("list containers", vec!["list"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::List(args)) => {
            passthrough_with_args("list containers", vec!["container", "ls"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Inspect(args)) => {
            passthrough_with_args_requiring_container_system(
                "inspect container",
                vec!["inspect"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Inspect(args)) => {
            passthrough_with_args("inspect container", vec!["container", "inspect"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Logs(args)) => {
            passthrough_with_args_requiring_container_system(
                "show container logs",
                vec!["logs"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Logs(args)) => {
            passthrough_with_args("show container logs", vec!["container", "logs"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Prune(args)) => {
            passthrough_with_args_requiring_container_system(
                "prune containers",
                vec!["prune"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Prune(args)) => {
            passthrough_with_args("prune containers", vec!["container", "prune"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Remove(args)) => {
            passthrough_with_args_requiring_container_system(
                "remove container",
                vec!["delete"],
                args,
            )
        }
        (ContainerRuntime::Podman, ContainerCommands::Remove(args)) => {
            passthrough_with_args("remove container", vec!["container", "rm"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Run(args)) => {
            passthrough_with_args_requiring_container_system("run container", vec!["run"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::Run(args)) => {
            passthrough_with_args("run container", vec!["container", "run"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Start(args)) => {
            passthrough_with_args_requiring_container_system("start container", vec!["start"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::Start(args)) => {
            passthrough_with_args("start container", vec!["container", "start"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Stats(args)) => {
            passthrough_with_args_requiring_container_system("container stats", vec!["stats"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::Stats(args)) => {
            passthrough_with_args("container stats", vec!["container", "stats"], args)
        }
        (ContainerRuntime::AppleContainer, ContainerCommands::Stop(args)) => {
            passthrough_with_args_requiring_container_system("stop container", vec!["stop"], args)
        }
        (ContainerRuntime::Podman, ContainerCommands::Stop(args)) => {
            passthrough_with_args("stop container", vec!["container", "stop"], args)
        }
    }
}

fn registry_passthrough(
    runtime: ContainerRuntime,
    command: RegistryCommands,
) -> Result<PassthroughInvocation, OrodruinError> {
    match (runtime, command) {
        (ContainerRuntime::AppleContainer, RegistryCommands::List(args)) => {
            Ok(passthrough_with_args_requiring_container_system(
                "list registries",
                vec!["registry", "list"],
                args,
            ))
        }
        (ContainerRuntime::Podman, RegistryCommands::List(_)) => Err(unsupported_with_podman(
            "registry list",
            "Podman does not provide an equivalent registry listing command",
        )),
        (ContainerRuntime::AppleContainer, RegistryCommands::Login(args)) => {
            Ok(passthrough_with_args_requiring_container_system(
                "login registry",
                vec!["registry", "login"],
                args,
            ))
        }
        (ContainerRuntime::Podman, RegistryCommands::Login(args)) => {
            Ok(passthrough_with_args("login registry", vec!["login"], args))
        }
        (ContainerRuntime::AppleContainer, RegistryCommands::Logout(args)) => {
            Ok(passthrough_with_args_requiring_container_system(
                "logout registry",
                vec!["registry", "logout"],
                args,
            ))
        }
        (ContainerRuntime::Podman, RegistryCommands::Logout(args)) => Ok(passthrough_with_args(
            "logout registry",
            vec!["logout"],
            args,
        )),
    }
}

fn builder_passthrough(
    runtime: ContainerRuntime,
    command: BuilderCommands,
) -> Result<PassthroughInvocation, OrodruinError> {
    match runtime {
        ContainerRuntime::AppleContainer => Ok(match command {
            BuilderCommands::Remove(args) => passthrough_with_args_requiring_container_system(
                "remove builder",
                vec!["builder", "delete"],
                args,
            ),
            BuilderCommands::Start(args) => passthrough_with_args_requiring_container_system(
                "start builder",
                vec!["builder", "start"],
                args,
            ),
            BuilderCommands::Status(args) => passthrough_with_args_requiring_container_system(
                "builder status",
                vec!["builder", "status"],
                args,
            ),
            BuilderCommands::Stop(args) => passthrough_with_args_requiring_container_system(
                "stop builder",
                vec!["builder", "stop"],
                args,
            ),
        }),
        ContainerRuntime::Podman => match command {
            BuilderCommands::Remove(args) => Ok(passthrough_with_args(
                "prune builder cache",
                vec!["builder", "prune"],
                args,
            )),
            BuilderCommands::Status(args) => Ok(passthrough_with_args(
                "inspect builder capabilities",
                vec!["builder", "inspect"],
                args,
            )),
            BuilderCommands::Start(_) => Err(unsupported_with_podman(
                "builder start",
                "Podman buildx does not provide a start subcommand",
            )),
            BuilderCommands::Stop(_) => Err(unsupported_with_podman(
                "builder stop",
                "Podman buildx does not provide a stop subcommand",
            )),
        },
    }
}

fn system_passthrough(
    runtime: ContainerRuntime,
    command: SystemCommands,
) -> Result<PassthroughInvocation, OrodruinError> {
    match runtime {
        ContainerRuntime::AppleContainer => Ok(match command {
            SystemCommands::Df(args) => passthrough_with_args_requiring_container_system(
                "system df",
                vec!["system", "df"],
                args,
            ),
            SystemCommands::Dns(args) => passthrough_with_args_requiring_container_system(
                "system dns",
                vec!["system", "dns"],
                args,
            ),
            SystemCommands::Kernel(args) => passthrough_with_args_requiring_container_system(
                "system kernel",
                vec!["system", "kernel"],
                args,
            ),
            SystemCommands::Logs(args) => passthrough_with_args_requiring_container_system(
                "system logs",
                vec!["system", "logs"],
                args,
            ),
            SystemCommands::Property(args) => passthrough_with_args_requiring_container_system(
                "system property",
                vec!["system", "property"],
                args,
            ),
            SystemCommands::Start(args) => {
                passthrough_with_args("start system", vec!["system", "start"], args)
            }
            SystemCommands::Status(args) => {
                passthrough_with_args("system status", vec!["system", "status"], args)
            }
            SystemCommands::Stop(args) => {
                passthrough_with_args("stop system", vec!["system", "stop"], args)
            }
            SystemCommands::Version(args) => {
                passthrough_with_args("system version", vec!["system", "version"], args)
            }
        }),
        ContainerRuntime::Podman => match command {
            SystemCommands::Df(args) => Ok(passthrough_with_args(
                "system df",
                vec!["system", "df"],
                args,
            )),
            SystemCommands::Logs(args) => Ok(passthrough_with_args(
                "system events",
                vec!["system", "events"],
                args,
            )),
            SystemCommands::Status(args) => Ok(passthrough_with_args(
                "system info",
                vec!["system", "info"],
                args,
            )),
            SystemCommands::Version(args) => Ok(passthrough_with_args(
                "podman version",
                vec!["version"],
                args,
            )),
            SystemCommands::Dns(_) => Err(unsupported_with_podman(
                "system dns",
                "Podman does not provide a system dns subcommand",
            )),
            SystemCommands::Kernel(_) => Err(unsupported_with_podman(
                "system kernel",
                "Podman does not provide a system kernel subcommand",
            )),
            SystemCommands::Property(_) => Err(unsupported_with_podman(
                "system property",
                "Podman does not provide a system property subcommand",
            )),
            SystemCommands::Start(_) => Err(unsupported_with_podman(
                "system start",
                "Podman does not provide a system start subcommand on Linux",
            )),
            SystemCommands::Stop(_) => Err(unsupported_with_podman(
                "system stop",
                "Podman does not provide a system stop subcommand on Linux",
            )),
        },
    }
}

fn machine_passthrough(
    runtime: ContainerRuntime,
    command: MachineCommands,
) -> Result<PassthroughInvocation, OrodruinError> {
    match runtime {
        ContainerRuntime::AppleContainer => Ok(match command {
            MachineCommands::Create(args) => passthrough_with_args_requiring_container_system(
                "create machine",
                vec!["machine", "create"],
                args,
            ),
            MachineCommands::Inspect(args) => passthrough_with_args_requiring_container_system(
                "inspect machine",
                vec!["machine", "inspect"],
                args,
            ),
            MachineCommands::List(args) => passthrough_with_args_requiring_container_system(
                "list machines",
                vec!["machine", "list"],
                args,
            ),
            MachineCommands::Logs(args) => passthrough_with_args_requiring_container_system(
                "machine logs",
                vec!["machine", "logs"],
                args,
            ),
            MachineCommands::Remove(args) => passthrough_with_args_requiring_container_system(
                "remove machine",
                vec!["machine", "delete"],
                args,
            ),
            MachineCommands::Run(args) => passthrough_with_args_requiring_container_system(
                "run machine command",
                vec!["machine", "run"],
                args,
            ),
            MachineCommands::Set(args) => passthrough_with_args_requiring_container_system(
                "set machine",
                vec!["machine", "set"],
                args,
            ),
            MachineCommands::SetDefault(args) => passthrough_with_args_requiring_container_system(
                "set default machine",
                vec!["machine", "set-default"],
                args,
            ),
            MachineCommands::Stop(args) => passthrough_with_args_requiring_container_system(
                "stop machine",
                vec!["machine", "stop"],
                args,
            ),
        }),
        ContainerRuntime::Podman => match command {
            MachineCommands::Create(args) => Ok(passthrough_with_args(
                "create machine",
                vec!["machine", "init"],
                args,
            )),
            MachineCommands::Inspect(args) => Ok(passthrough_with_args(
                "inspect machine",
                vec!["machine", "inspect"],
                args,
            )),
            MachineCommands::List(args) => Ok(passthrough_with_args(
                "list machines",
                vec!["machine", "list"],
                args,
            )),
            MachineCommands::Logs(args) => Ok(passthrough_with_args(
                "show machine info",
                vec!["machine", "info"],
                args,
            )),
            MachineCommands::Remove(args) => Ok(passthrough_with_args(
                "remove machine",
                vec!["machine", "rm"],
                args,
            )),
            MachineCommands::Run(args) => Ok(passthrough_with_args(
                "ssh into machine",
                vec!["machine", "ssh"],
                args,
            )),
            MachineCommands::Set(args) => Ok(passthrough_with_args(
                "set machine",
                vec!["machine", "set"],
                args,
            )),
            MachineCommands::SetDefault(_) => Err(unsupported_with_podman(
                "machine set-default",
                "Podman machine does not provide a set-default subcommand",
            )),
            MachineCommands::Stop(args) => Ok(passthrough_with_args(
                "stop machine",
                vec!["machine", "stop"],
                args,
            )),
        },
    }
}

fn unsupported_with_podman(command: &str, reason: &str) -> OrodruinError {
    OrodruinError::Message(format!(
        "`{command}` is not supported with the Linux Podman backend: {reason}"
    ))
}

fn doctor_check(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok,
        detail: detail.into(),
    }
}

fn doctor_environment_report(
    project_root: &Path,
    project: &ProjectConfig,
    env_name: &str,
    config: &crate::config::EnvironmentConfig,
) -> DoctorEnvironmentReport {
    let resolved = ResolvedEnvironment::resolve(project_root, project, env_name, config);
    let mut checks = vec![doctor_check(
        "project root mount",
        resolved.project_root.exists(),
        format!("host path {}", resolved.project_root.display()),
    )];

    if let Some(build) = &resolved.build {
        checks.push(doctor_check(
            "build context",
            build.context.exists(),
            format!("{}", build.context.display()),
        ));
        if let Some(file) = &build.file {
            checks.push(doctor_check(
                "build file",
                file.exists(),
                format!("{}", file.display()),
            ));
        }
    }

    for mount in &resolved.mounts[1..] {
        checks.push(doctor_check(
            format!("mount {}", mount.target),
            mount.source.exists(),
            format!("host path {}", mount.source.display()),
        ));
    }

    for key in &config.preserve_env {
        let present = env::var_os(key).is_some();
        checks.push(doctor_check(
            format!("preserve env {key}"),
            present,
            if present {
                String::from("available in host environment")
            } else {
                String::from("missing from host environment")
            },
        ));
    }

    let healthy = checks.iter().all(|check| check.ok);
    DoctorEnvironmentReport {
        env: env_name.to_string(),
        container: resolved.container_name,
        image: resolved.image,
        healthy,
        checks,
    }
}

fn runtime_name(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::AppleContainer => "apple-container",
        ContainerRuntime::Podman => "podman",
    }
}

fn runtime_program_display(runtime: ContainerRuntime) -> &'static str {
    match runtime {
        ContainerRuntime::AppleContainer => "Apple Container CLI",
        ContainerRuntime::Podman => "Podman",
    }
}

fn runtime_program_available(program: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|directory| executable_exists(&directory.join(program)))
}

fn executable_exists(path: &Path) -> bool {
    path.is_file()
}

impl From<OptionalPassthroughArgs> for Vec<String> {
    fn from(value: OptionalPassthroughArgs) -> Self {
        value.args
    }
}

impl From<RequiredPassthroughArgs> for Vec<String> {
    fn from(value: RequiredPassthroughArgs) -> Self {
        value.args
    }
}

fn print_doctor_report(report: &DoctorReport) {
    println!(
        "doctor: {}",
        if report.healthy {
            "ok"
        } else {
            "problems found"
        }
    );
    println!("runtime: {} ({})", report.runtime, report.runtime_program);
    if let Some(config_path) = &report.config_path {
        println!("config: {config_path}");
    }
    if let Some(project_root) = &report.project_root {
        println!("project root: {project_root}");
    }

    for check in &report.checks {
        println!(
            "check {}: {} ({})",
            check.name,
            if check.ok { "ok" } else { "fail" },
            check.detail
        );
    }

    for environment in &report.environments {
        println!(
            "env {}: {} [{}]",
            environment.env,
            if environment.healthy { "ok" } else { "fail" },
            environment.container
        );
        println!("  image: {}", environment.image);
        for check in &environment.checks {
            println!(
                "  check {}: {} ({})",
                check.name,
                if check.ok { "ok" } else { "fail" },
                check.detail
            );
        }
    }
}

fn prompt_strategy(auto_yes: bool) -> fn() -> Result<bool, OrodruinError> {
    if auto_yes {
        auto_yes_prompt
    } else {
        prompt_to_start_container_system
    }
}

fn auto_yes_prompt() -> Result<bool, OrodruinError> {
    Ok(true)
}

fn print_json<T: Serialize>(value: &T) -> Result<(), OrodruinError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| OrodruinError::Message(error.to_string()))?
    );
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
    fs::write(&path, default_init_config(project_name)?)?;
    println!("created {}", path.display());
    Ok(())
}

fn load_config(explicit_path: Option<&Path>) -> Result<LoadedConfig, OrodruinError> {
    let cwd = std::env::current_dir()?;
    Ok(ProjectConfig::load_from(&cwd, explicit_path)?)
}

fn configured_timeout_for_cli(cli: &Cli) -> Result<Option<Duration>, OrodruinError> {
    if let Some(timeout) = cli.timeout {
        return Ok(Some(timeout));
    }

    let loaded = match &cli.command {
        Commands::Create(_)
        | Commands::Enter(_)
        | Commands::Run(_)
        | Commands::List(_)
        | Commands::Rm(_)
        | Commands::Inspect(_)
        | Commands::Doctor(_) => Some(load_config(cli.config.as_deref())?),
        Commands::Init | Commands::Completions(_) | Commands::Version => None,
        _ => try_load_config(cli.config.as_deref())?,
    };

    let Some(loaded) = loaded else {
        return Ok(None);
    };

    let configured = match &cli.command {
        Commands::Create(environment) | Commands::Rm(environment) => {
            resolve_environment(&loaded, environment)?.timeout
        }
        Commands::Inspect(command) => resolve_environment_by_name(&loaded, &command.env)?.timeout,
        Commands::Enter(command) => {
            resolve_optional_environment(&loaded, command.env.as_deref(), "enter")?.timeout
        }
        Commands::Run(command) => {
            resolve_optional_environment(&loaded, command.env.as_deref(), "run")?.timeout
        }
        Commands::List(_)
        | Commands::Doctor(_)
        | Commands::Pull(_)
        | Commands::Images(_)
        | Commands::Rmi(_)
        | Commands::Ps(_)
        | Commands::Logs(_)
        | Commands::Build(_)
        | Commands::Copy(_)
        | Commands::Login(_)
        | Commands::Logout(_)
        | Commands::Image(_)
        | Commands::Container(_)
        | Commands::Registry(_)
        | Commands::Volume(_)
        | Commands::Network(_)
        | Commands::Builder(_)
        | Commands::System(_)
        | Commands::Machine(_) => loaded.config.project.default_timeout.clone(),
        Commands::Init | Commands::Completions(_) | Commands::Version => None,
    };

    configured
        .as_deref()
        .map(parse_timeout)
        .transpose()
        .map_err(OrodruinError::Message)
}

fn try_load_config(explicit_path: Option<&Path>) -> Result<Option<LoadedConfig>, OrodruinError> {
    let cwd = std::env::current_dir()?;
    match ProjectConfig::load_from(&cwd, explicit_path) {
        Ok(loaded) => Ok(Some(loaded)),
        Err(ConfigError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn doctor_command(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    explicit_path: Option<&Path>,
    command: DoctorCommand,
) -> Result<(), OrodruinError> {
    let report = doctor_report(backend, runtime, explicit_path)?;
    if command.json {
        print_json(&report)?;
    } else {
        print_doctor_report(&report);
    }
    if report.healthy {
        Ok(())
    } else {
        Err(OrodruinError::Message("doctor found problems".into()))
    }
}

fn doctor_report(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    explicit_path: Option<&Path>,
) -> Result<DoctorReport, OrodruinError> {
    let cwd = std::env::current_dir()?;
    let runtime_program = runtime.program().to_string();
    let mut checks = vec![doctor_check(
        "runtime binary",
        runtime_program_available(&runtime_program),
        format!("{} on PATH", runtime_program_display(runtime)),
    )];
    let config_path = match ProjectConfig::locate_path(&cwd, explicit_path) {
        Ok(path) => path,
        Err(error) => {
            checks.push(doctor_check("config file", false, error.to_string()));
            return Ok(DoctorReport {
                healthy: false,
                runtime: runtime_name(runtime).into(),
                runtime_program,
                config_path: None,
                project_root: None,
                checks,
                environments: vec![],
            });
        }
    };

    let loaded = match ProjectConfig::load_path(&config_path) {
        Ok(loaded) => {
            checks.push(doctor_check(
                "config file",
                true,
                format!("loaded {}", config_path.display()),
            ));
            loaded
        }
        Err(error) => {
            checks.push(doctor_check("config file", false, error.to_string()));
            return Ok(DoctorReport {
                healthy: false,
                runtime: runtime_name(runtime).into(),
                runtime_program,
                config_path: Some(config_path.display().to_string()),
                project_root: config_path.parent().map(|path| path.display().to_string()),
                checks,
                environments: vec![],
            });
        }
    };

    let runtime_available = checks.first().map(|check| check.ok).unwrap_or(false);
    if runtime.manages_system_lifecycle() {
        let system_check = if runtime_available {
            match backend.system_running() {
                Ok(true) => doctor_check("container system", true, "running"),
                Ok(false) => doctor_check(
                    "container system",
                    false,
                    "not running; start with `container system start` or rerun commands with `--yes`",
                ),
                Err(error) => doctor_check("container system", false, error.to_string()),
            }
        } else {
            doctor_check(
                "container system",
                false,
                "runtime binary missing; cannot check container system state",
            )
        };
        checks.push(system_check);
    }

    let environments = loaded
        .config
        .envs
        .iter()
        .map(|(name, config)| doctor_environment_report(&loaded.root, &loaded.config, name, config))
        .collect::<Vec<_>>();

    let healthy = checks.iter().all(|check| check.ok)
        && environments.iter().all(|environment| environment.healthy);

    Ok(DoctorReport {
        healthy,
        runtime: runtime_name(runtime).into(),
        runtime_program,
        config_path: Some(loaded.path.display().to_string()),
        project_root: Some(loaded.root.display().to_string()),
        checks,
        environments,
    })
}

fn resolve_environment(
    loaded: &LoadedConfig,
    environment: &EnvironmentName,
) -> Result<ResolvedEnvironment, OrodruinError> {
    resolve_environment_by_name(loaded, &environment.env)
}

fn resolve_optional_environment<'a>(
    loaded: &'a LoadedConfig,
    env_name: Option<&'a str>,
    command_name: &str,
) -> Result<ResolvedEnvironment, OrodruinError> {
    let env_name = match env_name {
        Some(env_name) => env_name,
        None => resolve_default_environment_name(loaded, command_name)?,
    };

    resolve_environment_by_name(loaded, env_name)
}

fn resolve_default_environment_name<'a>(
    loaded: &'a LoadedConfig,
    command_name: &str,
) -> Result<&'a str, OrodruinError> {
    if let Some(default_env) = loaded.config.project.default_env.as_deref() {
        return Ok(default_env);
    }

    if loaded.config.envs.len() == 1 {
        return Ok(loaded
            .config
            .envs
            .keys()
            .next()
            .expect("single environment checked above")
            .as_str());
    }

    Err(OrodruinError::Message(format!(
        "`orodruin {command_name}` needs an environment name; set project.default_env in {} or define only one environment",
        loaded.path.display(),
    )))
}

fn resolve_environment_by_name(
    loaded: &LoadedConfig,
    env_name: &str,
) -> Result<ResolvedEnvironment, OrodruinError> {
    let config = loaded.config.envs.get(env_name).ok_or_else(|| {
        OrodruinError::Message(format!(
            "environment `{}` is not defined in {}",
            env_name,
            loaded.path.display()
        ))
    })?;

    Ok(ResolvedEnvironment::resolve(
        &loaded.root,
        &loaded.config,
        env_name,
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

fn materialize_environment_for_runtime_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    resolved: &ResolvedEnvironment,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    materialize_environment(backend, resolved)
}

fn container_exists_for_runtime_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    container_name: &str,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<bool, OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    container_exists(backend, container_name)
}

fn list_environment_entries_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<Vec<ListEntry>, OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    let containers = backend.list_all()?;
    loaded
        .config
        .envs
        .iter()
        .map(|(name, config)| {
            let resolved = ResolvedEnvironment::resolve(&loaded.root, &loaded.config, name, config);
            let summary = containers
                .iter()
                .find(|summary| summary.matches(&resolved.container_name));
            Ok(summary_entry(name, &resolved.container_name, summary))
        })
        .collect()
}

fn list_environments_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    let entries = list_environment_entries_with_prompt(backend, runtime, loaded, prompt)?;
    for entry in entries {
        println!("{}\t{}\t{}", entry.env, entry.container, entry.state);
    }
    Ok(())
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

fn ensure_container_system_running_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    if !runtime.manages_system_lifecycle() {
        return Ok(());
    }

    if backend.system_running()? {
        return Ok(());
    }

    if prompt()? {
        backend.start_system()?;
        return Ok(());
    }

    Err(OrodruinError::Message(
        "container system is not running; start it with `container system start` or rerun with `--yes` to allow automatic startup".into(),
    ))
}

fn prompt_to_start_container_system() -> Result<bool, OrodruinError> {
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stderr = io::stderr();
    let mut stderr = stderr.lock();
    prompt_to_start_container_system_with_io(&mut stdin, &mut stderr)
}

fn prompt_to_start_container_system_with_io<R, W>(
    stdin: &mut R,
    stderr: &mut W,
) -> Result<bool, OrodruinError>
where
    R: BufRead,
    W: Write,
{
    loop {
        write!(
            stderr,
            "container system is not running. Start it now? [y/N] "
        )?;
        stderr.flush()?;

        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            return Ok(false);
        }

        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" | "" => return Ok(false),
            _ => {
                writeln!(stderr, "please answer y or n")?;
            }
        }
    }
}

fn ensure_container_user(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<(), OrodruinError> {
    backend.exec(
        &resolved.container_name,
        &ExecRequest {
            workdir: Some("/".into()),
            env: vec![
                ("ORODRUIN_HOST_USER".into(), resolved.user.username.clone()),
                ("ORODRUIN_HOST_UID".into(), resolved.user.uid.to_string()),
                ("ORODRUIN_HOST_GID".into(), resolved.user.gid.to_string()),
                ("ORODRUIN_HOST_HOME".into(), resolved.user.home.clone()),
            ],
            command: vec![
                "/bin/sh".into(),
                "-lc".into(),
                bootstrap_user_script().into(),
            ],
            interactive: false,
            user: Some(ResolvedUser {
                username: "root".into(),
                uid: 0,
                gid: 0,
                home: "/root".into(),
            }),
        },
    )?;
    Ok(())
}

fn parse_extra_env(vars: &[String]) -> Result<Vec<(String, String)>, OrodruinError> {
    let mut result = Vec::with_capacity(vars.len());
    for var in vars {
        let (key, value) = var.split_once('=').ok_or_else(|| {
            OrodruinError::Message(format!("invalid --env value `{var}`; expected KEY=VALUE"))
        })?;
        if key.is_empty() {
            return Err(OrodruinError::Message(
                "--env: variable name must not be empty".into(),
            ));
        }
        result.push((key.to_string(), value.to_string()));
    }
    Ok(result)
}

fn exec_environment(
    resolved: &ResolvedEnvironment,
    extra_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env = resolved
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    env.push(("HOME".into(), resolved.user.home.clone()));
    env.push(("USER".into(), resolved.user.username.clone()));
    env.push(("LOGNAME".into(), resolved.user.username.clone()));

    // Apply CLI --env overrides (highest priority, overrides resolved and HOME/USER/LOGNAME)
    for (key, value) in extra_env {
        if let Some(pos) = env.iter().position(|(k, _)| k == key) {
            env[pos] = (key.clone(), value.clone());
        } else {
            env.push((key.clone(), value.clone()));
        }
    }

    env
}

fn bootstrap_user_script() -> &'static str {
    r#"set -eu

username="${ORODRUIN_HOST_USER}"
uid="${ORODRUIN_HOST_UID}"
gid="${ORODRUIN_HOST_GID}"
home="${ORODRUIN_HOST_HOME}"

lookup_group_by_gid() {
    awk -F: -v gid="$1" '$3 == gid { print $1; exit }' /etc/group
}

lookup_user_by_name() {
    awk -F: -v name="$1" '$1 == name { print $3 ":" $4; exit }' /etc/passwd
}

lookup_user_by_uid() {
    awk -F: -v uid="$1" '$3 == uid { print $1; exit }' /etc/passwd
}

group_name="$(lookup_group_by_gid "$gid")"
if [ -z "$group_name" ]; then
    if command -v groupadd >/dev/null 2>&1; then
        groupadd -g "$gid" "$username"
        group_name="$username"
    elif command -v addgroup >/dev/null 2>&1; then
        addgroup -g "$gid" "$username"
        group_name="$username"
    else
        echo "missing groupadd/addgroup for gid ${gid}" >&2
        exit 1
    fi
fi

existing_user="$(lookup_user_by_name "$username")"
if [ -n "$existing_user" ]; then
    existing_uid="${existing_user%%:*}"
    existing_gid="${existing_user##*:}"
    if [ "$existing_uid" != "$uid" ] || [ "$existing_gid" != "$gid" ]; then
        echo "user ${username} exists with uid:gid ${existing_uid}:${existing_gid}, expected ${uid}:${gid}" >&2
        exit 1
    fi
else
    uid_owner="$(lookup_user_by_uid "$uid")"
    if [ -n "$uid_owner" ] && [ "$uid_owner" != "$username" ]; then
        username="$uid_owner"
        existing_user="$(lookup_user_by_name "$username")"
        existing_gid="${existing_user##*:}"
        if [ "$existing_gid" != "$gid" ]; then
            echo "uid ${uid} already owned by ${uid_owner} with gid ${existing_gid}, expected ${gid}" >&2
            exit 1
        fi
    else
        if command -v useradd >/dev/null 2>&1; then
            if useradd -K UID_MIN=0 -K UID_MAX=60000 -m -d "$home" -u "$uid" -g "$gid" -s /bin/sh "$username"; then
                :
            else
                useradd -m -d "$home" -u "$uid" -g "$gid" -s /bin/sh "$username"
            fi
        elif command -v adduser >/dev/null 2>&1; then
            adduser -D -h "$home" -u "$uid" -G "$group_name" "$username"
        else
            echo "missing useradd/adduser for ${username}" >&2
            exit 1
        fi
    fi
fi

mkdir -p "$home"
chown "$uid:$gid" "$home"
"#
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

fn inspect_environment_with_prompt<'a>(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    resolved: &'a ResolvedEnvironment,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<InspectReport<'a>, OrodruinError> {
    let container_exists = container_exists_for_runtime_with_prompt(
        backend,
        runtime,
        &resolved.container_name,
        prompt,
    )?;
    let inspect = if container_exists {
        backend.inspect_raw(&resolved.container_name)?
    } else {
        None
    };

    Ok(InspectReport {
        env: &resolved.environment_name,
        container: &resolved.container_name,
        image: &resolved.image,
        workdir: &resolved.workdir,
        container_exists,
        resolved,
        inspect,
    })
}

fn summary_entry(
    name: &str,
    container_name: &str,
    summary: Option<&ContainerSummary>,
) -> ListEntry {
    let (state, running, exists) = match summary {
        Some(summary) if summary.running => (String::from("running"), true, true),
        Some(summary) => (
            summary.status.as_deref().unwrap_or("created").to_string(),
            false,
            true,
        ),
        None => (String::from("not-created"), false, false),
    };

    ListEntry {
        env: name.to_string(),
        container: container_name.to_string(),
        state,
        running,
        exists,
    }
}

fn print_inspect_report(report: &InspectReport<'_>) {
    println!("env: {}", report.env);
    println!("container: {}", report.container);
    println!("image: {}", report.image);
    println!("workdir: {}", report.workdir);
    if !report.container_exists {
        println!("container not created");
        return;
    }

    match &report.inspect {
        Some(value) => println!(
            "{}",
            to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        ),
        None => println!("container not created"),
    }
}

#[cfg(test)]
mod tests {
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
            BuilderCommands, Commands, ContainerCommands, EnterCommand, ImageCommands,
            InspectCommand, ListCommand, MachineCommands, OptionalPassthroughArgs,
            RegistryCommands, ResourceCommands, SystemCommands,
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

        fn inspect_raw(
            &self,
            container_name: &str,
        ) -> Result<Option<serde_json::Value>, BackendError> {
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

        ensure_container_system_running_with_prompt(
            &backend,
            ContainerRuntime::AppleContainer,
            || Ok(true),
        )
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

        let entries = list_environment_entries_with_prompt(
            &backend,
            ContainerRuntime::Podman,
            &loaded,
            || Ok(true),
        )
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
        let resolved =
            resolve_environment(&loaded, &EnvironmentName { env: "dev".into() }).unwrap();

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
            execs[0].command[2].contains(
                "useradd -m -d \"$home\" -u \"$uid\" -g \"$gid\" -s /bin/sh \"$username\""
            )
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
}
