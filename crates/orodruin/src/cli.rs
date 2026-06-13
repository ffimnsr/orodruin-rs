use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "orodruin", version, about)]
pub struct Cli {
    #[arg(long, global = true, action = ArgAction::SetTrue)]
    pub debug: bool,
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    Init,
    Create(EnvironmentName),
    Enter(EnvironmentName),
    Run(RunCommand),
    List,
    Rm(EnvironmentName),
    Inspect(EnvironmentName),
}

#[derive(Debug, Args)]
pub struct EnvironmentName {
    pub env: String,
}

#[derive(Debug, Args)]
pub struct RunCommand {
    pub env: String,
    #[arg(last = true)]
    pub command: Vec<String>,
}
