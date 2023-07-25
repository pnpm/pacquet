mod commands;
mod fs;
mod package;
mod package_import;
mod package_manager;

use crate::package_manager::PackageManager;

use crate::commands::store::StoreSubcommands;
use crate::commands::{Cli, Subcommands};
use clap::Parser;
use pacquet_diagnostics::{
    enable_tracing_by_env,
    miette::{IntoDiagnostic, Result, WrapErr},
};
use pacquet_executor::execute_shell;
use pacquet_npmrc::get_current_npmrc;
use pacquet_package_json::PackageJson;

pub async fn run_cli() -> Result<()> {
    enable_tracing_by_env();
    let cli = Cli::parse();
    run_commands(cli).await
}

async fn run_commands(cli: Cli) -> Result<()> {
    let package_json_path = cli.current_dir.join("package.json");

    match &cli.subcommand {
        Subcommands::Init => {
            // init command throws an error if package.json file exist.
            PackageJson::init(&package_json_path).wrap_err("initialize package.json")?;
        }
        Subcommands::Add(args) => {
            let mut package_manager = PackageManager::new(&package_json_path)
                .wrap_err("initializing the package manager")?;
            // TODO if a package already exists in another dependency group, we don't remove
            // the existing entry.
            package_manager.add(args).await.wrap_err("adding a new package")?;
        }
        Subcommands::Install(args) => {
            let package_manager = PackageManager::new(&package_json_path)
                .wrap_err("initializing the package manager")?;
            package_manager
                .install(args)
                .await
                .into_diagnostic()
                .wrap_err("installing dependencies")?;
        }
        Subcommands::Test => {
            let package_json = PackageJson::from_path(&package_json_path)
                .wrap_err("getting the package.json in current directory")?;
            if let Some(script) = package_json.get_script("test", false)? {
                execute_shell(script).wrap_err(format!("executing command: \"{0}\"", script))?;
            }
        }
        Subcommands::Run(args) => {
            let package_json = PackageJson::from_path(&package_json_path)
                .wrap_err("getting the package.json in current directory")?;
            if let Some(script) = package_json.get_script(&args.command, args.if_present)? {
                let mut command = script.to_string();
                // append an empty space between script and additional args
                command.push(' ');
                // then append the additional args
                command.push_str(&args.args.join(" "));
                execute_shell(command.trim())?;
            }
        }
        Subcommands::Start => {
            // Runs an arbitrary command specified in the package's start property of its scripts
            // object. If no start property is specified on the scripts object, it will attempt to
            // run node server.js as a default, failing if neither are present.
            // The intended usage of the property is to specify a command that starts your program.
            let package_json = PackageJson::from_path(&package_json_path)
                .wrap_err("getting the package.json in current directory")?;
            let command = if let Some(script) = package_json.get_script("start", true)? {
                script
            } else {
                "node server.js"
            };
            execute_shell(command).wrap_err(format!("executing command: \"{0}\"", command))?;
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
                    pacquet_cafs::prune_sync(&config.store_dir).wrap_err("pruning store")?;
                }
                StoreSubcommands::Path => {
                    println!("{}", config.store_dir.display());
                }
            }
        }
        Subcommands::List(args) => {
            let group = args.get_scope();
            let depth = args.get_depth();
            PackageJson::from_path(&package_json_path)?.list(group, &node_modules_path, depth)?;
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
        let cli = Cli::parse_from(["", "-C", parent_folder.path().to_str().unwrap(), "init"]);
        run_commands(cli).await.unwrap();
        assert!(parent_folder.path().join("package.json").exists());
    }

    #[tokio::test]
    async fn init_command_should_throw_on_existing_file() {
        let parent_folder = tempdir().unwrap();
        let mut file = fs::File::create(parent_folder.path().join("package.json")).unwrap();
        file.write_all("{}".as_bytes()).unwrap();
        assert!(parent_folder.path().join("package.json").exists());
        let cli = Cli::parse_from(["", "-C", parent_folder.path().to_str().unwrap(), "init"]);
        run_commands(cli).await.expect_err("should have thrown");
    }

    #[tokio::test]
    async fn should_get_store_path() {
        let parent_folder = tempdir().unwrap();
        let cli =
            Cli::parse_from(["", "-C", parent_folder.path().to_str().unwrap(), "store", "path"]);
        run_commands(cli).await.unwrap();
    }
}
