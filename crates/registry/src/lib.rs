mod error;
mod package;

use std::{
    env,
    path::{Path, PathBuf},
};

use pacquet_tarball::download_and_extract;
use reqwest::Client;

use crate::{error::RegistryError, package::Package};

pub struct RegistryManager {
    client: Client,
    cache_directory: PathBuf,
}

impl RegistryManager {
    pub fn new<P: AsRef<Path>>(path: P) -> RegistryManager {
        RegistryManager { client: Client::new(), cache_directory: path.as_ref().to_owned() }
    }

    pub async fn get_package(&mut self, name: &String) -> Result<(), RegistryError> {
        let url = "https://registry.npmjs.com/".to_owned() + name;
        let package = Package::from_registry(&self.client, url.as_str()).await;
        let version_tag = package.get_latest_tag();
        let package_folder = self.cache_directory.join(&package.name);
        let node_modules = env::current_dir().unwrap().join("node_modules");
        let extract_destination = node_modules.join(package.get_latest_tag());

        if !package_folder.exists() {
            std::fs::create_dir_all(package_folder.as_path())
                .expect("package folder creation failed");
        }

        if !node_modules.exists() {
            std::fs::create_dir_all(&node_modules).expect("node_modules folder creation failed");
        }

        if !extract_destination.exists() {
            let _ = download_and_extract(
                &package.name,
                version_tag,
                package.get_tarball_url(),
                &self.cache_directory,
                node_modules.as_path(),
            )
            .await;
        }

        Ok(())
    }
}
