mod cli_args;
mod package_manager;

use clap::Parser;
use cli_args::{store::StoreCommand, CliArgs, CliCommand};
use miette::{set_panic_hook, Context, IntoDiagnostic};
use package_manager::PackageManager;
use pacquet_diagnostics::enable_tracing_by_env;
use pacquet_executor::execute_shell;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::PackageJson;

pub async fn main() -> miette::Result<()> {
    enable_tracing_by_env();
    set_panic_hook();
    let cli = CliArgs::parse();
    run(cli).await
}

async fn run(cli: CliArgs) -> miette::Result<()> {
    let package_json_path = cli.dir.join("package.json");

    match &cli.command {
        CliCommand::Init => {
            // init command throws an error if package.json file exist.
            PackageJson::init(&package_json_path).wrap_err("initialize package.json")?;
        }
        CliCommand::Add(args) => {
            let config = Npmrc::current().leak();
            let mut package_manager = PackageManager::new(&package_json_path, config)
                .wrap_err("initializing the package manager")?;
            // TODO if a package already exists in another dependency group, we don't remove
            // the existing entry.
            package_manager.add(args).await.wrap_err("adding a new package")?;
        }
        CliCommand::Install(args) => {
            let config = Npmrc::current().leak();
            let package_manager = PackageManager::new(&package_json_path, config)
                .wrap_err("initializing the package manager")?;
            package_manager
                .install(args)
                .await
                .into_diagnostic()
                .wrap_err("installing dependencies")?;
        }
        CliCommand::Test => {
            let package_json = PackageJson::from_path(package_json_path)
                .wrap_err("getting the package.json in current directory")?;
            if let Some(script) = package_json.script("test", false)? {
                execute_shell(script).wrap_err(format!("executing command: \"{0}\"", script))?;
            }
        }
        CliCommand::Run(args) => {
            let package_json = PackageJson::from_path(package_json_path)
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
            let package_json = PackageJson::from_path(package_json_path)
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
                let config = Npmrc::current().leak();
                pacquet_cafs::prune_sync(&config.store_dir).wrap_err("pruning store")?;
            }
            StoreCommand::Path => {
                let config = Npmrc::current().leak();
                println!("{}", config.store_dir.display());
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn should_get_store_path() {
        let parent_folder = tempdir().unwrap();
        let cli = CliArgs::parse_from([
            "",
            "-C",
            parent_folder.path().to_str().unwrap(),
            "store",
            "path",
        ]);
        run(cli).await.unwrap();
    }
}
