use clap_complete::generate;

use super::ensure_container_system_running_with_prompt;

use crate::{
    backend::{ContainerBackend, ContainerCliBackend, ContainerRuntime},
    cli::{
        BuilderCommands, Cli, CompletionsCommand, ContainerCommands, ImageCommands,
        MachineCommands, OptionalPassthroughArgs, RegistryCommands, RequiredPassthroughArgs,
        ResourceCommands, SystemCommands,
    },
    error::OrodruinError,
};

pub(crate) struct PassthroughInvocation {
    pub step: String,
    pub command: Vec<String>,
    pub requires_container_system: bool,
}

pub(crate) fn run_passthrough(
    backend: &dyn ContainerBackend,
    runtime: ContainerRuntime,
    invocation: PassthroughInvocation,
    prompt: fn() -> Result<bool, OrodruinError>,
) -> Result<(), OrodruinError> {
    run_passthrough_with_prompt(backend, runtime, invocation, prompt)
}

pub(crate) fn run_passthrough_with_prompt(
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

pub(crate) fn passthrough(step: impl Into<String>, command: Vec<String>) -> PassthroughInvocation {
    PassthroughInvocation {
        step: step.into(),
        command,
        requires_container_system: false,
    }
}

pub(crate) fn passthrough_requiring_container_system(
    step: impl Into<String>,
    command: Vec<String>,
) -> PassthroughInvocation {
    PassthroughInvocation {
        step: step.into(),
        command,
        requires_container_system: true,
    }
}

pub(crate) fn passthrough_with_args(
    step: impl Into<String>,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    let mut command = prefix.into_iter().map(str::to_string).collect::<Vec<_>>();
    command.extend(args.into());
    passthrough(step, command)
}

pub(crate) fn passthrough_with_args_requiring_container_system(
    step: impl Into<String>,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    let mut command = prefix.into_iter().map(str::to_string).collect::<Vec<_>>();
    command.extend(args.into());
    passthrough_requiring_container_system(step, command)
}

pub(crate) fn runtime_for_os(os: &str) -> Result<ContainerRuntime, OrodruinError> {
    ContainerRuntime::from_os(os).ok_or_else(|| {
        OrodruinError::Message(format!(
            "unsupported operating system `{os}`; supported platforms are macOS and Linux"
        ))
    })
}

pub(crate) fn render_completions(
    runtime: ContainerRuntime,
    command: CompletionsCommand,
) -> Result<String, OrodruinError> {
    let mut cli = Cli::command_for_runtime(runtime);
    let mut output = Vec::new();
    let name = cli.get_name().to_string();
    generate(command.shell, &mut cli, name, &mut output);
    String::from_utf8(output).map_err(|error| OrodruinError::Message(error.to_string()))
}

pub(crate) fn resource_passthrough(
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

pub(crate) fn passthrough_owned(
    step: String,
    prefix: Vec<&str>,
    args: impl Into<Vec<String>>,
) -> PassthroughInvocation {
    passthrough_with_args(step, prefix, args)
}

pub(crate) fn pull_passthrough(
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

pub(crate) fn images_passthrough(
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

pub(crate) fn rmi_passthrough(
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

pub(crate) fn ps_passthrough(
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

pub(crate) fn logs_passthrough(
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

pub(crate) fn build_passthrough(
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

pub(crate) fn copy_passthrough(
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

pub(crate) fn login_passthrough(
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

pub(crate) fn logout_passthrough(
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

pub(crate) fn image_passthrough(
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

pub(crate) fn container_passthrough(
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

pub(crate) fn registry_passthrough(
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

pub(crate) fn builder_passthrough(
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

pub(crate) fn system_passthrough(
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

pub(crate) fn machine_passthrough(
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

pub(crate) fn unsupported_with_podman(command: &str, reason: &str) -> OrodruinError {
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
