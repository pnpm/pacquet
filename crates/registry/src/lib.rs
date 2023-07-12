mod package;

use std::path::{Path, PathBuf};

use reqwest::Client;

use crate::package::{Error, Package};

pub struct RegistryManager {
    client: Client,
    cache_directory: PathBuf,
}

impl RegistryManager {
    pub fn new<P: AsRef<Path>>(path: P) -> RegistryManager {
        RegistryManager { client: Client::new(), cache_directory: path.as_ref().to_owned() }
    }

    pub async fn get_package(&self, name: &String) -> Result<(), Error> {
        let url = "https://registry.npmjs.com/".to_owned() + name;
        let package = Package::new(&self.client, url.as_str()).await;

        package.install_tarball(&self.cache_directory).await?;

        Ok(())
    }
}
