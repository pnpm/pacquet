mod commands;
mod tracing;

use std::env;

use anyhow::{Context, Result};
use clap::Parser;
use commands::{Cli, Subcommands};
use pacquet_executor::execute_shell;
use pacquet_npmrc::get_current_npmrc;
use pacquet_package_json::PackageJson;
use pacquet_package_manager::PackageManager;

use crate::{commands::StoreSubcommands, tracing::enable_tracing_by_env};

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
            PackageJson::init(&package_json_path).with_context(|| {
                format!("Failed to initialize package.json at path {:?}", &package_json_path)
            })?;
        }
        Subcommands::Add(args) => {
            let mut package_manager =
                PackageManager::new(&package_json_path).with_context(|| {
                    format!("Failed to read package.json at path {:?}", &package_json_path)
                })?;
            // TODO if a package already exists in another dependency group, we don't remove
            // the existing entry.
            package_manager
                .add(&args.package, args.get_dependency_group(), args.save_exact)
                .await
                .with_context(|| format!("Failed to add package {}", &args.package))?;
        }
        Subcommands::Install(args) => {
            let mut package_manager =
                PackageManager::new(&package_json_path).with_context(|| {
                    format!("Failed to read package.json at path {:?}", &package_json_path)
                })?;
            package_manager.install(args.dev, !args.no_optional).await.with_context(|| {
                format!("Failed to install packages on {:?}", package_json_path)
            })?;
        }
        Subcommands::Test => {
            let package_json = PackageJson::from_path(&package_json_path).with_context(|| {
                format!("Failed to read package.json at path {:?}", &package_json_path)
            })?;
            if let Some(script) = package_json.get_script("test", false)? {
                execute_shell(script)?;
            }
        }
        Subcommands::Run(args) => {
            let package_json = PackageJson::from_path(&package_json_path).with_context(|| {
                format!("Failed to read package.json at path {:?}", &package_json_path)
            })?;
            if let Some(script) = package_json.get_script(&args.command, args.if_present)? {
                execute_shell(script)?;
            }
        }
        Subcommands::Start => {
            // Runs an arbitrary command specified in the package's start property of its scripts
            // object. If no start property is specified on the scripts object, it will attempt to
            // run node server.js as a default, failing if neither are present.
            // The intended usage of the property is to specify a command that starts your program.
            let package_json = PackageJson::from_path(&package_json_path).with_context(|| {
                format!("Failed to read package.json at path {:?}", &package_json_path)
            })?;
            if let Some(script) = package_json.get_script("start", true)? {
                execute_shell(script)?;
            } else {
                execute_shell("node server.js")?;
            }
        }
        Subcommands::Store(subcommand) => {
            let config = get_current_npmrc();
            match subcommand {
                StoreSubcommands::Store => {
                    panic!("Not implemented")
                }
                StoreSubcommands::Add => {
                    panic!("Not implemented")
                }
                StoreSubcommands::Prune => {
                    panic!("Not implemented")
                }
                StoreSubcommands::Path => {
                    println!("{}", config.store_dir.display());
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn init_command_should_create_package_json() {
        let parent_folder = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(parent_folder.path()).unwrap();
        let cli = Cli::parse_from(["", "init"]);
        run_commands(cli).await.unwrap();
        assert!(parent_folder.path().join("package.json").exists());
        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    async fn init_command_should_throw_on_existing_file() {
        let parent_folder = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&parent_folder).unwrap();
        let mut file = fs::File::create(parent_folder.path().join("package.json")).unwrap();
        file.write_all("{}".as_bytes()).unwrap();
        assert!(parent_folder.path().join("package.json").exists());
        let cli = Cli::parse_from(["", "init"]);
        run_commands(cli).await.expect_err("should have thrown");
        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    async fn should_get_store_path() {
        let parent_folder = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(parent_folder.path()).unwrap();
        let cli = Cli::parse_from(["", "store", "path"]);
        run_commands(cli).await.unwrap();
        env::set_current_dir(&current_directory).unwrap();
    }
}
