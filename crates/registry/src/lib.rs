mod error;
mod package;

use std::{env, path::PathBuf};

use pacquet_tarball::download_and_extract;
use reqwest::Client;

use crate::{error::RegistryError, package::Package};

pub struct RegistryManager {
    client: Client,
    cache_directory: PathBuf,
}

impl RegistryManager {
    pub fn new<P: Into<PathBuf>>(path: P) -> RegistryManager {
        RegistryManager { client: Client::new(), cache_directory: path.into() }
    }

    pub async fn get_package(&mut self, name: &str) -> Result<(), RegistryError> {
        let url = format!("https://registry.npmjs.com/{name}");
        let package = Package::from_registry(&self.client, &url).await?;
        let version_tag = package.get_latest_tag()?;
        let package_folder = self.cache_directory.join(&package.name);
        let node_modules = env::current_dir()?.join("node_modules");
        let extract_destination = node_modules.join(package.get_latest_tag()?);

        std::fs::create_dir_all(package_folder.as_path())?;

        std::fs::create_dir_all(&node_modules)?;

        if !extract_destination.exists() {
            let _ = download_and_extract(
                &package.name,
                version_tag,
                package.get_tarball_url()?,
                &self.cache_directory,
                node_modules.as_path(),
            )
            .await;
        }

        Ok(())
    }
}
