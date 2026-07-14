use std::{ffi::OsString, path::PathBuf};

use clap::{ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use clap_complete::Shell;

use crate::backend::ContainerRuntime;

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "orodruin",
    version = crate::build_info::VERSION,
    long_version = crate::build_info::LONG_VERSION,
    about
)]
pub struct Cli {
    #[arg(long, global = true, action = ArgAction::SetTrue, help = "Enable debug logging")]
    pub debug: bool,
    #[arg(long, short = 'y', global = true, action = ArgAction::SetTrue, help = "Automatically answer yes to interactive prompts")]
    pub yes: bool,
    #[arg(long, global = true, help = "Path to the orodruin config file")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}

struct UnsupportedCommand {
    path: &'static [&'static str],
    hidden_name: &'static str,
}

const PODMAN_UNSUPPORTED_COMMANDS: &[UnsupportedCommand] = &[
    UnsupportedCommand {
        path: &["registry", "list"],
        hidden_name: "__hidden__registry__list",
    },
    UnsupportedCommand {
        path: &["builder", "start"],
        hidden_name: "__hidden__builder__start",
    },
    UnsupportedCommand {
        path: &["builder", "stop"],
        hidden_name: "__hidden__builder__stop",
    },
    UnsupportedCommand {
        path: &["system", "dns"],
        hidden_name: "__hidden__system__dns",
    },
    UnsupportedCommand {
        path: &["system", "kernel"],
        hidden_name: "__hidden__system__kernel",
    },
    UnsupportedCommand {
        path: &["system", "property"],
        hidden_name: "__hidden__system__property",
    },
    UnsupportedCommand {
        path: &["system", "start"],
        hidden_name: "__hidden__system__start",
    },
    UnsupportedCommand {
        path: &["system", "stop"],
        hidden_name: "__hidden__system__stop",
    },
    UnsupportedCommand {
        path: &["machine", "set-default"],
        hidden_name: "__hidden__machine__set_default",
    },
];

impl Cli {
    pub fn parse_for_runtime<I, T>(args: I, runtime: ContainerRuntime) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        Self::parse_for_runtime_named(args, runtime, "orodruin")
    }

    pub fn parse_for_runtime_named<I, T>(
        args: I,
        runtime: ContainerRuntime,
        program_name: &str,
    ) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let matches = Self::parsing_command_for_runtime_named(runtime, program_name)
            .try_get_matches_from(args)?;
        Self::from_arg_matches(&matches)
    }

    pub fn command_for_runtime(runtime: ContainerRuntime) -> Command {
        Self::command_for_runtime_named(runtime, "orodruin")
    }

    pub fn command_for_runtime_named(runtime: ContainerRuntime, program_name: &str) -> Command {
        prune_command_for_runtime(Self::base_command_named(program_name), runtime, &[])
    }

    fn parsing_command_for_runtime_named(runtime: ContainerRuntime, program_name: &str) -> Command {
        hide_unsupported_commands_for_runtime(Self::base_command_named(program_name), runtime, &[])
    }

    fn base_command_named(program_name: &str) -> Command {
        Self::command().bin_name(program_name)
    }
}

fn prune_command_for_runtime(
    command: Command,
    runtime: ContainerRuntime,
    parent_path: &[String],
) -> Command {
    let mut path = parent_path.to_vec();
    path.push(command.get_name().to_string());

    if let Some(unsupported) = unsupported_command(runtime, &path) {
        return command
            .name(unsupported.hidden_name)
            .aliases(Vec::<&str>::new())
            .visible_aliases(Vec::<&str>::new())
            .hide(true);
    }

    command.mut_subcommands(|child| prune_command_for_runtime(child, runtime, &path))
}

fn hide_unsupported_commands_for_runtime(
    command: Command,
    runtime: ContainerRuntime,
    parent_path: &[String],
) -> Command {
    let mut path = parent_path.to_vec();
    path.push(command.get_name().to_string());

    if unsupported_command(runtime, &path).is_some() {
        return command.hide(true);
    }

    command.mut_subcommands(|child| hide_unsupported_commands_for_runtime(child, runtime, &path))
}

fn unsupported_command(
    runtime: ContainerRuntime,
    path: &[String],
) -> Option<&'static UnsupportedCommand> {
    if runtime == ContainerRuntime::AppleContainer {
        return None;
    }

    let relative_path = path.iter().skip(1).map(String::as_str).collect::<Vec<_>>();
    PODMAN_UNSUPPORTED_COMMANDS
        .iter()
        .find(|command| command.path == relative_path.as_slice())
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Commands {
    #[command(about = "Create a starter orodruin.toml in the current directory")]
    Init,
    #[command(about = "Create the container for an environment and start it if needed")]
    Create(EnvironmentName),
    #[command(about = "Open an interactive shell inside an environment container")]
    Enter(EnterCommand),
    #[command(about = "Run a command in an environment container")]
    Run(RunCommand),
    #[command(about = "List configured environments and their container state")]
    List(ListCommand),
    #[command(about = "Remove an environment container")]
    Rm(EnvironmentName),
    #[command(about = "Show the resolved configuration and container details for an environment")]
    Inspect(InspectCommand),
    #[command(about = "Pull an image with the configured container runtime")]
    Pull(RequiredPassthroughArgs),
    #[command(about = "List local images with the configured container runtime")]
    Images(OptionalPassthroughArgs),
    #[command(about = "Remove an image with the configured container runtime")]
    Rmi(RequiredPassthroughArgs),
    #[command(about = "List containers with the configured container runtime")]
    Ps(OptionalPassthroughArgs),
    #[command(about = "Show container logs with the configured container runtime")]
    Logs(OptionalPassthroughArgs),
    #[command(about = "Build an image with the configured container runtime")]
    Build(RequiredPassthroughArgs),
    #[command(
        about = "Copy files with the configured container runtime",
        visible_alias = "cp"
    )]
    Copy(RequiredPassthroughArgs),
    #[command(about = "Log in to a registry with the configured container runtime")]
    Login(OptionalPassthroughArgs),
    #[command(about = "Log out from a registry with the configured container runtime")]
    Logout(OptionalPassthroughArgs),
    #[command(
        subcommand,
        about = "Run image commands with the configured container runtime"
    )]
    Image(ImageCommands),
    #[command(
        subcommand,
        about = "Run container commands with the configured container runtime"
    )]
    Container(ContainerCommands),
    #[command(
        subcommand,
        about = "Run registry commands with the configured container runtime"
    )]
    Registry(RegistryCommands),
    #[command(
        subcommand,
        about = "Run volume commands with the configured container runtime"
    )]
    Volume(ResourceCommands),
    #[command(
        subcommand,
        about = "Run network commands with the configured container runtime"
    )]
    Network(ResourceCommands),
    #[command(
        subcommand,
        about = "Run builder commands with the configured container runtime"
    )]
    Builder(BuilderCommands),
    #[command(
        subcommand,
        about = "Run system commands with the configured container runtime"
    )]
    System(SystemCommands),
    #[command(
        subcommand,
        about = "Run machine commands with the configured container runtime"
    )]
    Machine(MachineCommands),
    #[command(about = "Generate shell completion scripts")]
    Completions(CompletionsCommand),
    #[command(about = "Show version, commit, date, and build information")]
    Version,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct CompletionsCommand {
    #[arg(value_enum, help = "Shell to generate completions for")]
    pub shell: Shell,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct EnvironmentName {
    #[arg(help = "Environment name from orodruin.toml")]
    pub env: String,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct ListCommand {
    #[arg(long, action = ArgAction::SetTrue, help = "Print structured JSON output")]
    pub json: bool,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct InspectCommand {
    #[arg(help = "Environment name from orodruin.toml")]
    pub env: String,
    #[arg(long, action = ArgAction::SetTrue, help = "Print structured JSON output")]
    pub json: bool,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct EnterCommand {
    #[arg(
        help = "Environment name from orodruin.toml; defaults from project.default_env or the sole environment"
    )]
    pub env: Option<String>,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct RunCommand {
    #[arg(
        help = "Environment name from orodruin.toml; defaults from project.default_env or the sole environment"
    )]
    pub env: Option<String>,
    #[arg(
        last = true,
        help = "Command to execute after `--`; uses the environment default if omitted"
    )]
    pub command: Vec<String>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct OptionalPassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct RequiredPassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    pub args: Vec<String>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ImageCommands {
    #[command(about = "Pull an image")]
    Pull(RequiredPassthroughArgs),
    #[command(name = "list", about = "List local images", visible_alias = "ls")]
    List(OptionalPassthroughArgs),
    #[command(about = "Inspect an image", visible_alias = "i")]
    Inspect(RequiredPassthroughArgs),
    #[command(about = "Load an image")]
    Load(OptionalPassthroughArgs),
    #[command(name = "remove", about = "Remove an image", visible_alias = "rm")]
    Remove(RequiredPassthroughArgs),
    #[command(about = "Push an image")]
    Push(RequiredPassthroughArgs),
    #[command(about = "Remove unused images")]
    Prune(OptionalPassthroughArgs),
    #[command(about = "Save an image")]
    Save(OptionalPassthroughArgs),
    #[command(about = "Tag an image")]
    Tag(RequiredPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ContainerCommands {
    #[command(about = "Create a container")]
    Create(OptionalPassthroughArgs),
    #[command(about = "Execute a command in a container")]
    Exec(OptionalPassthroughArgs),
    #[command(about = "Export a container")]
    Export(OptionalPassthroughArgs),
    #[command(about = "Kill a container")]
    Kill(OptionalPassthroughArgs),
    #[command(name = "list", about = "List containers", visible_alias = "ls")]
    List(OptionalPassthroughArgs),
    #[command(about = "Inspect a container", visible_alias = "i")]
    Inspect(RequiredPassthroughArgs),
    #[command(about = "Show container logs")]
    Logs(OptionalPassthroughArgs),
    #[command(about = "Remove stopped containers")]
    Prune(OptionalPassthroughArgs),
    #[command(name = "remove", about = "Remove a container", visible_alias = "rm")]
    Remove(RequiredPassthroughArgs),
    #[command(about = "Run a container")]
    Run(OptionalPassthroughArgs),
    #[command(about = "Start a container")]
    Start(RequiredPassthroughArgs),
    #[command(about = "Show container stats")]
    Stats(OptionalPassthroughArgs),
    #[command(about = "Stop a container")]
    Stop(RequiredPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum RegistryCommands {
    #[command(
        name = "list",
        about = "List configured registries",
        visible_alias = "ls"
    )]
    List(OptionalPassthroughArgs),
    #[command(about = "Log in to a registry")]
    Login(OptionalPassthroughArgs),
    #[command(about = "Log out from a registry")]
    Logout(OptionalPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum ResourceCommands {
    #[command(name = "list", about = "List resources", visible_alias = "ls")]
    List(OptionalPassthroughArgs),
    #[command(about = "Create a resource", visible_alias = "c")]
    Create(RequiredPassthroughArgs),
    #[command(about = "Inspect a resource", visible_alias = "i")]
    Inspect(RequiredPassthroughArgs),
    #[command(about = "Remove unused resources")]
    Prune(OptionalPassthroughArgs),
    #[command(name = "remove", about = "Remove a resource", visible_alias = "rm")]
    Remove(RequiredPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum BuilderCommands {
    #[command(name = "remove", about = "Remove the builder", visible_alias = "rm")]
    Remove(OptionalPassthroughArgs),
    #[command(about = "Start the builder")]
    Start(OptionalPassthroughArgs),
    #[command(about = "Show builder status")]
    Status(OptionalPassthroughArgs),
    #[command(about = "Stop the builder")]
    Stop(OptionalPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum SystemCommands {
    #[command(about = "Show system disk usage")]
    Df(OptionalPassthroughArgs),
    #[command(about = "Manage system DNS")]
    Dns(OptionalPassthroughArgs),
    #[command(about = "Manage kernel configuration")]
    Kernel(OptionalPassthroughArgs),
    #[command(about = "Show system logs")]
    Logs(OptionalPassthroughArgs),
    #[command(about = "Manage system properties")]
    Property(OptionalPassthroughArgs),
    #[command(about = "Start container services")]
    Start(OptionalPassthroughArgs),
    #[command(about = "Show service status")]
    Status(OptionalPassthroughArgs),
    #[command(about = "Stop container services")]
    Stop(OptionalPassthroughArgs),
    #[command(about = "Show system version")]
    Version(OptionalPassthroughArgs),
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum MachineCommands {
    #[command(about = "Create a container machine")]
    Create(OptionalPassthroughArgs),
    #[command(about = "Inspect a container machine")]
    Inspect(OptionalPassthroughArgs),
    #[command(name = "list", about = "List container machines", visible_alias = "ls")]
    List(OptionalPassthroughArgs),
    #[command(about = "Show machine logs")]
    Logs(OptionalPassthroughArgs),
    #[command(
        name = "remove",
        about = "Remove a container machine",
        visible_alias = "rm"
    )]
    Remove(OptionalPassthroughArgs),
    #[command(about = "Run a command in a container machine")]
    Run(OptionalPassthroughArgs),
    #[command(about = "Set machine configuration")]
    Set(OptionalPassthroughArgs),
    #[command(name = "set-default", about = "Set the default container machine")]
    SetDefault(OptionalPassthroughArgs),
    #[command(about = "Stop a container machine")]
    Stop(OptionalPassthroughArgs),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_aliases() {
        let cli = Cli::parse_from(["orodruin", "images"]);
        assert_eq!(
            cli.command,
            Commands::Images(OptionalPassthroughArgs { args: vec![] })
        );

        let cli = Cli::parse_from(["orodruin", "image", "ls"]);
        assert_eq!(
            cli.command,
            Commands::Image(ImageCommands::List(OptionalPassthroughArgs {
                args: vec![]
            }))
        );

        let cli = Cli::parse_from(["orodruin", "rmi", "alpine:latest"]);
        assert_eq!(
            cli.command,
            Commands::Rmi(RequiredPassthroughArgs {
                args: vec!["alpine:latest".into()],
            })
        );

        let cli = Cli::parse_from(["orodruin", "image", "i", "alpine:latest"]);
        assert_eq!(
            cli.command,
            Commands::Image(ImageCommands::Inspect(RequiredPassthroughArgs {
                args: vec!["alpine:latest".into()],
            }))
        );

        let cli = Cli::parse_from(["orodruin", "image", "load", "-i", "image.tar"]);
        assert_eq!(
            cli.command,
            Commands::Image(ImageCommands::Load(OptionalPassthroughArgs {
                args: vec!["-i".into(), "image.tar".into()],
            }))
        );
    }

    #[test]
    fn parses_passthrough_aliases() {
        let cli = Cli::parse_from(["orodruin", "cp", "src", "dst"]);
        assert_eq!(
            cli.command,
            Commands::Copy(RequiredPassthroughArgs {
                args: vec!["src".into(), "dst".into()],
            })
        );

        let cli = Cli::parse_from(["orodruin", "registry", "ls"]);
        assert_eq!(
            cli.command,
            Commands::Registry(RegistryCommands::List(OptionalPassthroughArgs {
                args: vec![]
            }))
        );

        let cli = Cli::parse_from(["orodruin", "volume", "c", "cache"]);
        assert_eq!(
            cli.command,
            Commands::Volume(ResourceCommands::Create(RequiredPassthroughArgs {
                args: vec!["cache".into()],
            }))
        );

        let cli = Cli::parse_from(["orodruin", "network", "i", "bridge"]);
        assert_eq!(
            cli.command,
            Commands::Network(ResourceCommands::Inspect(RequiredPassthroughArgs {
                args: vec!["bridge".into()],
            }))
        );

        let cli = Cli::parse_from(["orodruin", "container", "i", "demo"]);
        assert_eq!(
            cli.command,
            Commands::Container(ContainerCommands::Inspect(RequiredPassthroughArgs {
                args: vec!["demo".into()],
            }))
        );

        let cli = Cli::parse_from(["orodruin", "container", "prune", "--all"]);
        assert_eq!(
            cli.command,
            Commands::Container(ContainerCommands::Prune(OptionalPassthroughArgs {
                args: vec!["--all".into()],
            }))
        );

        let cli = Cli::parse_from(["orodruin", "machine", "ls"]);
        assert_eq!(
            cli.command,
            Commands::Machine(MachineCommands::List(OptionalPassthroughArgs {
                args: vec![]
            }))
        );

        let cli = Cli::parse_from(["orodruin", "builder", "rm"]);
        assert_eq!(
            cli.command,
            Commands::Builder(BuilderCommands::Remove(OptionalPassthroughArgs {
                args: vec![]
            }))
        );

        let cli = Cli::parse_from(["orodruin", "system", "version"]);
        assert_eq!(
            cli.command,
            Commands::System(SystemCommands::Version(OptionalPassthroughArgs {
                args: vec![]
            }))
        );
    }

    #[test]
    fn parses_completions_subcommand() {
        let cli = Cli::parse_from(["orodruin", "completions", "zsh"]);
        assert_eq!(
            cli.command,
            Commands::Completions(CompletionsCommand { shell: Shell::Zsh })
        );
    }

    #[test]
    fn parses_global_yes_and_list_json() {
        let cli = Cli::parse_from(["orodruin", "--yes", "list", "--json"]);
        assert!(cli.yes);
        assert_eq!(cli.command, Commands::List(ListCommand { json: true }));
    }

    #[test]
    fn parses_inspect_json() {
        let cli = Cli::parse_from(["orodruin", "inspect", "dev", "--json"]);
        assert_eq!(
            cli.command,
            Commands::Inspect(InspectCommand {
                env: "dev".into(),
                json: true,
            })
        );
    }

    #[test]
    fn parses_run_without_environment() {
        let cli = Cli::parse_from(["orodruin", "run"]);
        assert_eq!(
            cli.command,
            Commands::Run(RunCommand {
                env: None,
                command: vec![],
            })
        );

        let cli = Cli::parse_from(["orodruin", "run", "dev"]);
        assert_eq!(
            cli.command,
            Commands::Run(RunCommand {
                env: Some("dev".into()),
                command: vec![],
            })
        );

        let cli = Cli::parse_from(["orodruin", "run", "dev", "--", "date"]);
        assert_eq!(
            cli.command,
            Commands::Run(RunCommand {
                env: Some("dev".into()),
                command: vec!["date".into()],
            })
        );
    }

    #[test]
    fn parses_enter_without_environment() {
        let cli = Cli::parse_from(["orodruin", "enter"]);
        assert_eq!(cli.command, Commands::Enter(EnterCommand { env: None }));

        let cli = Cli::parse_from(["orodruin", "enter", "dev"]);
        assert_eq!(
            cli.command,
            Commands::Enter(EnterCommand {
                env: Some("dev".into()),
            })
        );
    }

    #[test]
    fn podman_runtime_prunes_unsupported_subcommands() {
        let command = Cli::command_for_runtime(ContainerRuntime::Podman);
        let registry = command.find_subcommand("registry").unwrap();
        let builder = command.find_subcommand("builder").unwrap();
        let system = command.find_subcommand("system").unwrap();
        let machine = command.find_subcommand("machine").unwrap();

        assert!(registry.find_subcommand("list").is_none());
        assert!(registry.find_subcommand("login").is_some());
        assert!(builder.find_subcommand("start").is_none());
        assert!(builder.find_subcommand("status").is_some());
        assert!(system.find_subcommand("dns").is_none());
        assert!(system.find_subcommand("version").is_some());
        assert!(machine.find_subcommand("set-default").is_none());
        assert!(machine.find_subcommand("set").is_some());
    }

    #[test]
    fn command_for_runtime_named_uses_the_requested_binary_name() {
        let command = Cli::command_for_runtime_named(ContainerRuntime::Podman, "rui");

        assert_eq!(command.get_bin_name(), Some("rui"));
    }

    #[test]
    fn podman_runtime_parses_hidden_unsupported_subcommands() {
        let cli = Cli::parse_for_runtime(["orodruin", "system", "dns"], ContainerRuntime::Podman)
            .unwrap();
        assert_eq!(
            cli.command,
            Commands::System(SystemCommands::Dns(OptionalPassthroughArgs {
                args: vec![]
            }))
        );

        let cli =
            Cli::parse_for_runtime(["orodruin", "registry", "list"], ContainerRuntime::Podman)
                .unwrap();
        assert_eq!(
            cli.command,
            Commands::Registry(RegistryCommands::List(OptionalPassthroughArgs {
                args: vec![]
            }))
        );
    }
}
