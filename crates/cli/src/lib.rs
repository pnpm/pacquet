mod commands;

use anyhow::{Context, Result};
use clap::Parser;
use commands::{Cli, Subcommands};
use pacquet_package_json::PackageJson;
use pacquet_registry::RegistryManager;

pub async fn run_commands() -> Result<()> {
    let current_directory =
        std::env::current_dir().context("problem fetching current directory")?;
    let cache_directory = current_directory.join(".pacquet").as_path().to_owned();
    let node_modules = current_directory.join("node_modules").as_path().to_owned();

    if !cache_directory.exists() {
        std::fs::create_dir(&cache_directory).context("cache folder creation failed")?;
    }

    if !node_modules.exists() {
        std::fs::create_dir(&node_modules).context("node_modules folder creation failed")?;
    }

    let mut registry_manager = RegistryManager::new(cache_directory);

    let cli = Cli::parse();

    match &cli.subcommand {
        Subcommands::Init => {
            let pkg = PackageJson::from_current_directory();
            pkg.create_if_needed();
        }
        Subcommands::Add(args) => {
            registry_manager.get_package(&args.package).await?;
        }
    }

    Ok(())
}
