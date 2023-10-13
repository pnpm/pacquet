pub mod add;
pub mod install;
pub mod run;
pub mod store;

use add::AddArgs;
use clap::{Parser, Subcommand};
use install::InstallArgs;
use run::RunArgs;
use std::{env, ffi::OsString, path::PathBuf};
use store::StoreCommand;

fn default_current_dir() -> OsString {
    env::current_dir().expect("failed to get current directory").into_os_string()
}

/// Experimental package manager for node.js written in rust.
#[derive(Parser, Debug)]
#[command(name = "pacquet")]
#[command(bin_name = "pacquet")]
#[command(version = "0.2.1")]
#[command(about = "Experimental package manager for node.js")]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: CliCommand,

    /// Set working directory.
    #[arg(short = 'C', long = "dir", default_value = default_current_dir())]
    pub dir: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Initialize a package.json
    Init,
    /// Add a package
    Add(AddArgs),
    /// Install packages
    Install(InstallArgs),
    /// Runs a package's "test" script, if one was provided.
    Test,
    /// Runs a defined package script.
    Run(RunArgs),
    /// Runs an arbitrary command specified in the package's start property of its scripts object.
    Start,
    /// Managing the package store.
    #[clap(subcommand)]
    Store(StoreCommand),
}
