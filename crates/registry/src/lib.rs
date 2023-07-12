mod package;

use std::env;
use std::path::Path;

use reqwest::Client;

use crate::package::{Error, Package};

pub struct RegistryManager<'a> {
    client: Client,
    cache_directory: &'a Path,
}

impl Default for RegistryManager<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl RegistryManager<'_> {
    pub fn new() -> RegistryManager<'static> {
        let cache_directory = env::current_dir().unwrap().join(".pacquet");
        if !cache_directory.exists() {
            std::fs::create_dir(cache_directory).expect("failed to create cache directory");
        }
        RegistryManager { client: Client::new(), cache_directory: cache_directory.to_owned().as_path() }
    }

    pub async fn get_package(&self, name: &String) -> Result<(), Error> {
        let url = "https://registry.npmjs.com/".to_owned() + name;
        let package = Package::new(&self.client, url.as_str()).await;

        package.install_tarball(self.cache_directory).await?;

        Ok(())
    }
}
