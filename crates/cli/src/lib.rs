mod commands;

use std::env;

use pacquet_registry::RegistryManager;

use crate::commands::get_commands;

pub async fn run_commands() {
    let matches = get_commands().get_matches();
    let cache_directory = env::current_dir().unwrap().join(".pacquet").as_path().to_owned();
    if !cache_directory.exists() {
        std::fs::create_dir(&cache_directory).expect("failed to create cache directory");
    }
    let registry_manager = RegistryManager::new(cache_directory);

    if let Some(subcommand) = matches.subcommand_matches("add") {
        if let Some(package_name) = subcommand.get_one::<String>("package") {
            registry_manager.get_package(package_name).await.expect("TODO: panic message");
        }
    }
}

pub fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_commands())
}
