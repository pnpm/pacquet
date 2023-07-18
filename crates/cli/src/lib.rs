mod commands;
mod tracing;

use std::env;

use anyhow::{Context, Result};
use clap::Parser;
use commands::{Cli, Subcommands};
use pacquet_package_json::PackageJson;
use pacquet_registry::RegistryManager;

use crate::tracing::{enable_tracing_by_env, enable_tracing_by_env_with_chrome_layer};

pub async fn run_commands() -> Result<()> {
    enable_tracing_by_env();
    enable_tracing_by_env_with_chrome_layer();
    let current_directory = env::current_dir().context("problem fetching current directory")?;
    let package_json_path = current_directory.join("package.json");
    let cli = Cli::parse();

    match &cli.subcommand {
        Subcommands::Init => {
            // init command throws an error if package.json file exist.
            PackageJson::init(&package_json_path)?;
        }
        Subcommands::Add(args) => {
            let mut registry_manager = RegistryManager::new(
                current_directory.join("node_modules"),
                current_directory.join(&args.virtual_store_dir),
                package_json_path,
            )?;
            registry_manager.prepare()?;
            // TODO if a package already exists in another dependency group, we don't remove
            // the existing entry.
            registry_manager.add_dependency(&args.package, args.get_dependency_group()).await?;
        }
    }

    Ok(())
}
