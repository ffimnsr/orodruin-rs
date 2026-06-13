use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
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

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(about = "Create a starter orodruin.toml in the current directory")]
    Init,
    #[command(about = "Create the container for an environment and start it if needed")]
    Create(EnvironmentName),
    #[command(about = "Open an interactive shell inside an environment container")]
    Enter(EnvironmentName),
    #[command(about = "Run a command in an environment container")]
    Run(RunCommand),
    #[command(about = "List configured environments and their container state")]
    List,
    #[command(about = "Remove an environment container")]
    Rm(EnvironmentName),
    #[command(about = "Show the resolved configuration and container details for an environment")]
    Inspect(EnvironmentName),
    #[command(about = "Show version, commit, date, and build information")]
    Version,
}

#[derive(Debug, Args)]
pub struct EnvironmentName {
    #[arg(help = "Environment name from orodruin.toml")]
    pub env: String,
}

#[derive(Debug, Args)]
pub struct RunCommand {
    #[arg(help = "Environment name from orodruin.toml")]
    pub env: String,
    #[arg(
        last = true,
        help = "Command to execute after `--`; uses the environment default if omitted"
    )]
    pub command: Vec<String>,
}
