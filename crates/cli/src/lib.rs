mod commands;
mod tracing;

use std::env;

use anyhow::{Context, Result};
use clap::Parser;
use commands::{Cli, Subcommands};
use pacquet_package_json::PackageJson;
use pacquet_registry::RegistryManager;

use crate::tracing::enable_tracing_by_env;

pub async fn run_cli() -> Result<()> {
    enable_tracing_by_env();
    let cli = Cli::parse();
    run_commands(cli).await?;
    Ok(())
}

async fn run_commands(cli: Cli) -> Result<()> {
    let current_directory = env::current_dir().context("problem fetching current directory")?;
    let package_json_path = current_directory.join("package.json");

    match &cli.subcommand {
        Subcommands::Init => {
            // init command throws an error if package.json file exist.
            PackageJson::init(&package_json_path)?;
        }
        Subcommands::Add(args) => {
            let mut registry_manager = RegistryManager::new(package_json_path)?;
            // TODO if a package already exists in another dependency group, we don't remove
            // the existing entry.
            registry_manager.add_dependency(&args.package, args.get_dependency_group()).await?;
        }
        Subcommands::Test => {
            PackageJson::from_path(&package_json_path)?.execute_command("test")?;
        }
        Subcommands::RunScript(args) => {
            let command = &args.command;
            PackageJson::from_path(&package_json_path)?.execute_command(command)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, path::PathBuf};

    use uuid::Uuid;

    use super::*;

    fn prepare() -> PathBuf {
        let parent_folder = env::temp_dir().join(Uuid::new_v4().to_string());
        fs::create_dir_all(&parent_folder).unwrap();
        env::set_current_dir(&parent_folder).unwrap();
        parent_folder
    }

    #[tokio::test]
    async fn init_command_should_create_package_json() {
        let current_directory = env::current_dir().unwrap();
        let parent_folder = prepare();
        let cli = Cli::parse_from(["", "init"]);
        run_commands(cli).await.unwrap();

        assert!(parent_folder.join("package.json").exists());
        env::set_current_dir(&current_directory).unwrap();
        fs::remove_dir_all(&parent_folder).unwrap();
    }

    #[tokio::test]
    async fn init_command_should_throw_on_existing_file() {
        let current_directory = env::current_dir().unwrap();
        let parent_folder = prepare();
        let mut file = fs::File::create(parent_folder.join("package.json")).unwrap();
        file.write_all("{}".as_bytes()).unwrap();
        assert!(parent_folder.join("package.json").exists());
        let cli = Cli::parse_from(["", "init"]);
        run_commands(cli).await.expect_err("should have thrown");
        env::set_current_dir(&current_directory).unwrap();
        fs::remove_dir_all(&parent_folder).unwrap();
    }
}
