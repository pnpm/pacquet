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
use pacquet_npmrc::{Npmrc, RealApi};
use pacquet_package_manifest::PackageManifest;
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
        let manifest_path = || dir.join("package.json");
        // Production callers turbofish `RealApi` explicitly so the
        // dependency-injection plumbing is visible at the call site.
        // See `plans/PORTING_GUIDE.md` for the pattern and rationale.
        let npmrc = || {
            Npmrc::current::<RealApi, _, _, _, _>(
                env::current_dir,
                home::home_dir,
                Default::default,
            )
            .leak()
        };
        // `require_lockfile` is the "this subcommand cannot run without a
        // lockfile loaded" signal, used by `State::init` to override
        // `config.lockfile=false`. Only `install --frozen-lockfile` needs
        // it today; other subcommands follow `config.lockfile`. Matches
        // pnpm's CLI: `--frozen-lockfile` is the strongest signal and
        // must not be silently dropped because `lockfile=false` was set
        // (or defaulted) in config.
        let state = |require_lockfile: bool| {
            State::init(manifest_path(), npmrc(), require_lockfile).wrap_err("initialize the state")
        };

        match command {
            CliCommand::Init => {
                PackageManifest::init(&manifest_path()).wrap_err("initialize package.json")?;
            }
            CliCommand::Add(args) => args.run(state(false)?).await?,
            CliCommand::Install(args) => {
                let require_lockfile = args.frozen_lockfile;
                args.run(state(require_lockfile)?).await?
            }
            CliCommand::Test => {
                let manifest = PackageManifest::from_path(manifest_path())
                    .wrap_err("getting the package.json in current directory")?;
                if let Some(script) = manifest.script("test", false)? {
                    execute_shell(script)
                        .wrap_err(format!("executing command: \"{0}\"", script))?;
                }
            }
            CliCommand::Run(args) => args.run(manifest_path())?,
            CliCommand::Start => {
                // Runs an arbitrary command specified in the package's start property of its scripts
                // object. If no start property is specified on the scripts object, it will attempt to
                // run node server.js as a default, failing if neither are present.
                // The intended usage of the property is to specify a command that starts your program.
                let manifest = PackageManifest::from_path(manifest_path())
                    .wrap_err("getting the package.json in current directory")?;
                let command = manifest.script("start", true)?.unwrap_or("node server.js");
                execute_shell(command).wrap_err(format!("executing command: \"{0}\"", command))?;
            }
            CliCommand::Store(command) => command.run(|| npmrc())?,
        }

        Ok(())
    }
}
