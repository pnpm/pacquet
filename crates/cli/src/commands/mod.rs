pub mod add;
pub mod install;
pub mod list;
pub mod run;
pub mod store;

use std::{env, ffi::OsString, path::PathBuf};

use crate::commands::{
    add::AddCommandArgs, install::InstallCommandArgs, list::ListArgs, run::RunCommandArgs,
    store::StoreSubcommands,
};
use clap::{Parser, Subcommand};

fn default_current_dir() -> OsString {
    env::current_dir().expect("failed to get current directory").into_os_string()
}

/// Experimental package manager for node.js written in rust.
#[derive(Parser, Debug)]
#[command(name = "pacquet")]
#[command(bin_name = "pacquet")]
#[command(version = "0.1.3")]
#[command(about = "Experimental package manager for node.js")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Subcommands,

    /// Run as if pacquet was started in <path> instead of the current working directory.
    #[arg(short = 'C', long = "dir", default_value = default_current_dir())]
    pub current_dir: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum Subcommands {
    /// Initialize a package.json
    Init,
    /// Add a package
    Add(AddCommandArgs),
    /// Install packages
    Install(InstallCommandArgs),
    /// Runs a package's "test" script, if one was provided.
    Test,
    /// Runs a defined package script.
    Run(RunCommandArgs),
    /// Runs an arbitrary command specified in the package's start property of its scripts object.
    Start,
    /// Managing the package store.
    #[clap(subcommand)]
    Store(StoreSubcommands),
    /// List all dependencies installed based on package.json
    List(ListArgs),
}
