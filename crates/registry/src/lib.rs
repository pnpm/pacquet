mod error;
mod package;
mod package_name;
mod version_pin;

use std::path::PathBuf;

use pacquet_tarball::download_and_extract;
use reqwest::Client;

use crate::{error::RegistryError, package::Package};

pub struct RegistryManager {
    client: Client,
    node_modules_path: PathBuf,
    store_path: PathBuf,
}

impl RegistryManager {
    pub fn new<P: Into<PathBuf>>(path: P) -> RegistryManager {
        let path_into = path.into();
        RegistryManager {
            client: Client::new(),
            node_modules_path: path_into.clone(),
            store_path: path_into.join(".pacquet"),
        }
    }

    pub async fn get_package(&mut self, name: &str) -> Result<(), RegistryError> {
        let url = format!("https://registry.npmjs.com/{name}");
        let package = Package::from_registry(&self.client, &url).await?;
        let version_tag = package.get_latest_tag()?;

        // create store path.
        std::fs::create_dir_all(&self.store_path)?;

        download_and_extract(
            &package.name,
            version_tag,
            package.get_tarball_url()?,
            &self.store_path,
            &self.node_modules_path,
        )
        .await?;

        Ok(())
    }
}
