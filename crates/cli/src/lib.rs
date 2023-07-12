mod commands;

use pacquet_registry::RegistryManager;

use crate::commands::get_commands;

pub async fn run_commands() {
    let matches = get_commands().get_matches();
    let registry_manager = RegistryManager::new();

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
