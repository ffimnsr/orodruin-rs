use std::{
    ffi::OsString,
    fs,
    io::{self, BufRead, Write},
    net::IpAddr,
    path::Path,
    process::{Command as ProcessCommand, Stdio},
};

use clap_complete::generate;
use serde_json::{Value, to_string_pretty};

use crate::{
    backend::{
        BackendError, CommandSpec, ContainerBackend, ContainerCliBackend, ContainerRuntime,
        ExecRequest,
    },
    cli::{
        BuilderCommands, Cli, Commands, CompletionCli, CompletionsCommand, ContainerCommands,
        EnvironmentName, ImageCommands, MachineCommands, OptionalPassthroughArgs, RegistryCommands,
        RequiredPassthroughArgs, ResourceCommands, RunCommand, SystemCommands,
    },
    config::{CONFIG_FILE_NAME, LoadedConfig, ProjectConfig, default_init_config},
    env_model::{ResolvedEnvironment, ResolvedUser},
    error::OrodruinError,
    state::ContainerSummary,
};

struct PassthroughInvocation {
    step: String,
    command: Vec<String>,
    requires_container_system: bool,
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
    let backend = ContainerCliBackend::new(cli.debug, runtime);
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
    match cli.command {
        Commands::Init => init_command()?,
        Commands::Create(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment_for_runtime(backend, runtime, &resolved)?;
            println!("ready {}", resolved.container_name);
        }
        Commands::Enter(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved =
                resolve_optional_environment(&loaded, environment.env.as_deref(), "enter")?;
            materialize_environment_for_runtime(backend, runtime, &resolved)?;
            ensure_container_user(backend, &resolved)?;
            match backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved),
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
        Commands::Ssh(command) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_optional_environment(&loaded, command.env.as_deref(), "ssh")?;
            materialize_environment_for_runtime(backend, runtime, &resolved)?;
            ensure_container_user(backend, &resolved)?;
            let target = resolve_ssh_target(backend, &resolved)?;
            let spec = build_ssh_spec(&resolved, &target);
            if command.print {
                println!("{}", spec.render());
            } else {
                run_host_command(&spec)?;
            }
        }
        Commands::Run(run) => {
            let loaded = load_config(cli.config.as_deref())?;
            let environment = EnvironmentName {
                env: run.env.clone(),
            };
            let resolved = resolve_environment(&loaded, &environment)?;
            materialize_environment_for_runtime(backend, runtime, &resolved)?;
            let command = resolve_run_command(&resolved, run)?;
            ensure_container_user(backend, &resolved)?;
            backend.exec(
                &resolved.container_name,
                &ExecRequest {
                    workdir: Some(resolved.workdir.clone()),
                    env: exec_environment(&resolved),
                    command,
                    interactive: false,
                    user: Some(resolved.user.clone()),
                },
            )?;
        }
        Commands::List => {
            let loaded = load_config(cli.config.as_deref())?;
            list_environments(backend, runtime, &loaded)?;
        }
        Commands::Rm(environment) => {
            let loaded = load_config(cli.config.as_deref())?;
            let resolved = resolve_environment(&loaded, &environment)?;
            if !container_exists_for_runtime(backend, runtime, &resolved.container_name)? {
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
            if !container_exists_for_runtime(backend, runtime, &resolved.container_name)? {
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
        Commands::Pull(args) => run_passthrough(backend, runtime, pull_passthrough(runtime, args))?,
        Commands::Images(args) => {
            run_passthrough(backend, runtime, images_passthrough(runtime, args))?
        }
        Commands::Rmi(args) => run_passthrough(backend, runtime, rmi_passthrough(runtime, args))?,
        Commands::Ps(args) => run_passthrough(backend, runtime, ps_passthrough(runtime, args))?,
        Commands::Logs(args) => run_passthrough(backend, runtime, logs_passthrough(runtime, args))?,
        Commands::Build(args) => {
            run_passthrough(backend, runtime, build_passthrough(runtime, args))?
        }
        Commands::Copy(args) => run_passthrough(backend, runtime, copy_passthrough(runtime, args))?,
        Commands::Login(args) => {
            run_passthrough(backend, runtime, login_passthrough(runtime, args))?
        }
        Commands::Logout(args) => {
            run_passthrough(backend, runtime, logout_passthrough(runtime, args))?
        }
        Commands::Image(command) => {
            run_passthrough(backend, runtime, image_passthrough(runtime, command)?)?
        }
        Commands::Container(command) => {
            run_passthrough(backend, runtime, container_passthrough(runtime, command))?
        }
        Commands::Registry(command) => {
            run_passthrough(backend, runtime, registry_passthrough(runtime, command)?)?
        }
        Commands::Volume(command) => run_passthrough(
            backend,
            runtime,
            resource_passthrough(runtime, "volume", command),
        )?,
        Commands::Network(command) => run_passthrough(
            backend,
            runtime,
            resource_passthrough(runtime, "network", command),
        )?,
        Commands::Builder(command) => {
            run_passthrough(backend, runtime, builder_passthrough(runtime, command)?)?
        }
        Commands::System(command) => {
            run_passthrough(backend, runtime, system_passthrough(runtime, command)?)?
        }
        Commands::Machine(command) => {
            run_passthrough(backend, runtime, machine_passthrough(runtime, command)?)?
        }
        Commands::Completions(command) => print!("{}", render_completions(runtime, command)?),
        Commands::Version => println!("{}", crate::build_info::render()),
    }

    Ok(())
}

fn run_passthrough(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    invocation: PassthroughInvocation,
) -> Result<(), OrodruinError> {
    run_passthrough_with_prompt(
        backend,
        runtime,
        invocation,
        prompt_to_start_container_system,
    )
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
    let mut cli = CompletionCli::command_for_runtime(runtime);
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

fn resolve_ssh_target(
    backend: &dyn ContainerBackend,
    resolved: &ResolvedEnvironment,
) -> Result<String, OrodruinError> {
    let inspect = backend
        .inspect_raw(&resolved.container_name)?
        .ok_or_else(|| {
            OrodruinError::Message(format!(
                "container `{}` is running but inspect returned no payload",
                resolved.container_name
            ))
        })?;

    ssh_host_from_inspect(&inspect).ok_or_else(|| {
        OrodruinError::Message(format!(
            "could not determine an ssh host for `{}` from container inspect output",
            resolved.container_name
        ))
    })
}

fn ssh_host_from_inspect(value: &Value) -> Option<String> {
    if let Some(host) = value
        .get("status")
        .and_then(|status| status.get("networks"))
        .and_then(Value::as_array)
        .and_then(|networks| {
            networks.iter().find_map(|network| {
                find_ip_field(network, &["ipv4Address", "ipAddress", "address"])
            })
        })
    {
        return Some(host);
    }

    find_ip_field(
        value,
        &["ipv4Address", "ipAddress", "ip_address", "IPAddress", "ip"],
    )
}

fn find_ip_field(value: &Value, field_names: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for field_name in field_names {
                if let Some(candidate) = map.get(*field_name).and_then(Value::as_str)
                    && let Some(host) = normalize_ip(candidate)
                {
                    return Some(host);
                }
            }

            map.values()
                .find_map(|nested| find_ip_field(nested, field_names))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| find_ip_field(nested, field_names)),
        Value::String(candidate) => normalize_ip(candidate),
        _ => None,
    }
}

fn normalize_ip(candidate: &str) -> Option<String> {
    let host = candidate.split('/').next()?.trim();
    host.parse::<IpAddr>().ok().map(|_| host.to_string())
}

fn build_ssh_spec(resolved: &ResolvedEnvironment, host: &str) -> CommandSpec {
    CommandSpec {
        program: "ssh".into(),
        args: vec![format!("{}@{host}", resolved.user.username)],
    }
}

fn run_host_command(spec: &CommandSpec) -> Result<(), OrodruinError> {
    let status = ProcessCommand::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        return Ok(());
    }

    Err(OrodruinError::CommandFailed {
        command: spec.render(),
        status: status.code(),
    })
}

fn materialize_environment_for_runtime(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    resolved: &ResolvedEnvironment,
) -> Result<(), OrodruinError> {
    materialize_environment_for_runtime_with_prompt(
        backend,
        runtime,
        resolved,
        prompt_to_start_container_system,
    )
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

fn container_exists_for_runtime(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    container_name: &str,
) -> Result<bool, OrodruinError> {
    container_exists_for_runtime_with_prompt(
        backend,
        runtime,
        container_name,
        prompt_to_start_container_system,
    )
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

fn list_environments(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
) -> Result<(), OrodruinError> {
    list_environments_with_prompt(backend, runtime, loaded, prompt_to_start_container_system)
}

fn list_environments_with_prompt(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    loaded: &LoadedConfig,
    prompt: impl FnOnce() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    ensure_container_system_running_with_prompt(backend, runtime, prompt)?;
    let containers = backend.list_all()?;
    for (name, config) in &loaded.config.envs {
        let resolved = ResolvedEnvironment::resolve(&loaded.root, &loaded.config, name, config);
        let summary = containers
            .iter()
            .find(|summary| summary.matches(&resolved.container_name));
        print_summary(name, &resolved.container_name, summary);
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
        "container system not running; start it first with `container system start`".into(),
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

fn exec_environment(resolved: &ResolvedEnvironment) -> Vec<(String, String)> {
    let mut env = resolved
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Vec<_>>();
    env.push(("HOME".into(), resolved.user.home.clone()));
    env.push(("USER".into(), resolved.user.username.clone()));
    env.push(("LOGNAME".into(), resolved.user.username.clone()));
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
        backend::{BackendError, CommandSpec},
        cli::{
            BuilderCommands, Commands, ContainerCommands, EnterCommand, ImageCommands,
            MachineCommands, OptionalPassthroughArgs, RegistryCommands, ResourceCommands,
            SshCommand, SystemCommands,
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
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
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
        assert!(error.to_string().contains("container system not running"));
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
                stderr: String::new(),
            }),
        ]);

        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
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
            backend.execs.borrow()[1].command,
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
                default_env: None,
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

    #[test]
    fn enter_bootstraps_user_before_shell_exec() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Enter(EnterCommand {
                env: Some("dev".into()),
            }),
        };

        run_with_backend(cli, &backend).unwrap();

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
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.builds.borrow().as_slice(), ["demo-ci:dev"]);
    }

    #[test]
    fn ssh_print_uses_project_default_env_when_omitted() {
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
        let container_name = resolved_name(tempdir.path(), "ci");

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.inspect_value.borrow_mut() = Some(json!({
            "status": {
                "networks": [{ "ipv4Address": "192.168.64.2/24" }]
            }
        }));
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: None,
                print: true,
            }),
        };

        run_with_backend(cli, &backend).unwrap();
        assert_eq!(backend.builds.borrow().as_slice(), ["demo-ci:dev"]);
        assert_eq!(backend.inspect_calls.borrow().as_slice(), [container_name]);
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
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
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
            config: None,
            command: Commands::Enter(EnterCommand { env: None }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(error.to_string().contains("set project.default_env"));
    }

    #[test]
    fn ssh_without_env_errors_when_ambiguous() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::default();
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: None,
                print: true,
            }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("`orodruin ssh` needs an environment name")
        );
    }

    #[test]
    fn ssh_host_uses_network_ipv4_address_without_cidr() {
        let host = ssh_host_from_inspect(&json!({
            "status": {
                "networks": [{
                    "ipv4Address": "192.168.64.2/24",
                    "ipv6Address": "fdc8:9ac1:53c8:9dd2:f85b:11ff:fe51:30c9/64"
                }]
            }
        }));

        assert_eq!(host.as_deref(), Some("192.168.64.2"));
    }

    #[test]
    fn ssh_print_errors_when_inspect_has_no_ip() {
        let tempdir = tempfile::tempdir().unwrap();
        write_config(tempdir.path());
        let _guard = CurrentDirGuard::enter(tempdir.path());

        let backend = MockBackend::with_lists(vec![vec![]]);
        *backend.inspect_value.borrow_mut() = Some(json!({ "status": { "state": "running" } }));
        let cli = Cli {
            debug: false,
            config: None,
            command: Commands::Ssh(SshCommand {
                env: Some("dev".into()),
                print: true,
            }),
        };

        let error = run_with_backend(cli, &backend).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("could not determine an ssh host")
        );
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
}
