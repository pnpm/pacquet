pub mod add;
pub mod install;
pub mod run;
pub mod store;

use crate::State;
use add::AddArgs;
use clap::{Parser, Subcommand};
use install::InstallArgs;
use miette::Context;
use pacquet_executor::execute_shell;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::PackageJson;
use run::RunArgs;
use std::{env, path::PathBuf};
use store::StoreCommand;

/// Experimental package manager for node.js written in rust.
#[derive(Debug, Parser)]
#[clap(name = "pacquet")]
#[clap(bin_name = "pacquet")]
#[clap(version = "0.2.1")]
#[clap(about = "Experimental package manager for node.js")]
pub struct CliArgs {
    #[clap(subcommand)]
    pub command: CliCommand,

    /// Set working directory.
    #[clap(short = 'C', long, default_value = ".")]
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

impl CliArgs {
    /// Execute the command
    pub async fn run(self) -> miette::Result<()> {
        let CliArgs { command, dir } = self;
        let package_json_path = || dir.join("package.json");
        let npmrc = || Npmrc::current(env::current_dir, home::home_dir, Default::default).leak();
        let state = || State::init(package_json_path(), npmrc()).wrap_err("initialize the state");

        match command {
            CliCommand::Init => {
                // init command throws an error if package.json file exist.
                PackageJson::init(&package_json_path()).wrap_err("initialize package.json")?;
            }
            CliCommand::Add(args) => return args.run(state()?).await,
            CliCommand::Install(args) => return args.run(state()?).await,
            CliCommand::Test => {
                let package_json = PackageJson::from_path(package_json_path())
                    .wrap_err("getting the package.json in current directory")?;
                if let Some(script) = package_json.script("test", false)? {
                    execute_shell(script)
                        .wrap_err(format!("executing command: \"{0}\"", script))?;
                }
            }
            CliCommand::Run(args) => {
                let package_json = PackageJson::from_path(package_json_path())
                    .wrap_err("getting the package.json in current directory")?;
                if let Some(script) = package_json.script(&args.command, args.if_present)? {
                    let mut command = script.to_string();
                    // append an empty space between script and additional args
                    command.push(' ');
                    // then append the additional args
                    command.push_str(&args.args.join(" "));
                    execute_shell(command.trim())?;
                }
            }
            CliCommand::Start => {
                // Runs an arbitrary command specified in the package's start property of its scripts
                // object. If no start property is specified on the scripts object, it will attempt to
                // run node server.js as a default, failing if neither are present.
                // The intended usage of the property is to specify a command that starts your program.
                let package_json = PackageJson::from_path(package_json_path())
                    .wrap_err("getting the package.json in current directory")?;
                let command = if let Some(script) = package_json.script("start", true)? {
                    script
                } else {
                    "node server.js"
                };
                execute_shell(command).wrap_err(format!("executing command: \"{0}\"", command))?;
            }
            CliCommand::Store(command) => match command {
                StoreCommand::Store => {
                    panic!("Not implemented")
                }
                StoreCommand::Add => {
                    panic!("Not implemented")
                }
                StoreCommand::Prune => {
                    let config = npmrc();
                    pacquet_cafs::prune_sync(&config.store_dir).wrap_err("pruning store")?;
                }
                StoreCommand::Path => {
                    let config = npmrc();
                    println!("{}", config.store_dir.display());
                }
            },
        }

        Ok(())
    }
}
