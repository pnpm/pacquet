mod commands;

use pacquet_package_json::PackageJson;
use pacquet_registry::RegistryManager;

use crate::commands::get_commands;

pub async fn run_commands() {
    let matches = get_commands().get_matches();
    let current_directory = std::env::current_dir().expect("current directory should exist");
    let cache_directory = current_directory.join(".pacquet").as_path().to_owned();
    let node_modules = current_directory.join("node_modules").as_path().to_owned();

    if !cache_directory.exists() {
        std::fs::create_dir(&cache_directory).expect("cache folder creation failed");
    }

    if !node_modules.exists() {
        std::fs::create_dir(&node_modules).expect("node_modules folder creation failed");
    }

    let mut registry_manager = RegistryManager::new(cache_directory);

    if let Some(subcommand) = matches.subcommand_matches("add") {
        if let Some(package_name) = subcommand.get_one::<String>("package") {
            registry_manager.get_package(package_name).await.expect("TODO: panic message");
        }
    } else if matches.subcommand_matches("init").is_some() {
        let pkg = PackageJson::from_current_directory();
        pkg.create_if_needed();
    }
}
