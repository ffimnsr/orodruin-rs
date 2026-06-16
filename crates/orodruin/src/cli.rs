use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};
use clap_complete::Shell;

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
    #[arg(long, global = true, help = "Path to the orodruin config file")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Commands {
    #[command(about = "Create a starter orodruin.toml in the current directory")]
    Init,
    #[command(about = "Create the container for an environment and start it if needed")]
    Create(EnvironmentName),
    #[command(about = "Open an interactive shell inside an environment container")]
    Enter(EnterCommand),
    #[command(about = "SSH to an environment using the container network address")]
    Ssh(SshCommand),
    #[command(about = "Run a command in an environment container")]
    Run(RunCommand),
    #[command(about = "List configured environments and their container state")]
    List,
    #[command(about = "Remove an environment container")]
    Rm(EnvironmentName),
    #[command(about = "Show the resolved configuration and container details for an environment")]
    Inspect(EnvironmentName),
    #[command(about = "Pull an image with Apple container")]
    Pull(RequiredPassthroughArgs),
    #[command(about = "List local images with Apple container")]
    Images(OptionalPassthroughArgs),
    #[command(about = "Remove an image with Apple container")]
    Rmi(RequiredPassthroughArgs),
    #[command(about = "List containers with Apple container")]
    Ps(OptionalPassthroughArgs),
    #[command(about = "Show container logs with Apple container")]
    Logs(OptionalPassthroughArgs),
    #[command(about = "Build an image with Apple container")]
    Build(RequiredPassthroughArgs),
    #[command(about = "Copy files with Apple container", visible_alias = "cp")]
    Copy(RequiredPassthroughArgs),
    #[command(about = "Log in to a registry with Apple container")]
    Login(OptionalPassthroughArgs),
    #[command(about = "Log out from a registry with Apple container")]
    Logout(OptionalPassthroughArgs),
    #[command(subcommand, about = "Run Apple container image commands")]
    Image(ImageCommands),
    #[command(subcommand, about = "Run Apple container container commands")]
    Container(ContainerCommands),
    #[command(subcommand, about = "Run Apple container registry commands")]
    Registry(RegistryCommands),
    #[command(subcommand, about = "Run Apple container volume commands")]
    Volume(ResourceCommands),
    #[command(subcommand, about = "Run Apple container network commands")]
    Network(ResourceCommands),
    #[command(subcommand, about = "Run Apple container builder commands")]
    Builder(BuilderCommands),
    #[command(subcommand, about = "Run Apple container system commands")]
    System(SystemCommands),
    #[command(subcommand, about = "Run Apple container machine commands")]
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
pub struct EnterCommand {
    #[arg(
        help = "Environment name from orodruin.toml; defaults from project.default_env or the sole environment"
    )]
    pub env: Option<String>,
}

#[derive(Debug, Args, Clone, PartialEq, Eq)]
pub struct SshCommand {
    #[arg(
        help = "Environment name from orodruin.toml; defaults from project.default_env or the sole environment"
    )]
    pub env: Option<String>,
    #[arg(long, action = ArgAction::SetTrue, help = "Print the ssh command instead of executing it")]
    pub print: bool,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct RunCommand {
    #[arg(help = "Environment name from orodruin.toml")]
    pub env: String,
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
    fn parses_ssh_without_environment() {
        let cli = Cli::parse_from(["orodruin", "ssh", "--print"]);
        assert_eq!(
            cli.command,
            Commands::Ssh(SshCommand {
                env: None,
                print: true,
            })
        );

        let cli = Cli::parse_from(["orodruin", "ssh", "dev"]);
        assert_eq!(
            cli.command,
            Commands::Ssh(SshCommand {
                env: Some("dev".into()),
                print: false,
            })
        );
    }
}
