mod error;
mod package;
mod package_name;
mod version_pin;

use std::{fs, path::PathBuf};

use futures_util::{future::join_all};
use pacquet_tarball::download_and_extract;
use reqwest::Client;

use crate::{error::RegistryError, package::Package, version_pin::parse_version};

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

    pub fn prepare(&self) -> Result<(), RegistryError> {
        // create store path.
        fs::create_dir_all(&self.store_path)?;
        Ok(())
    }

    pub async fn add_dependency(&mut self, name: &str) -> Result<(), RegistryError> {
        let url = format!("https://registry.npmjs.com/{name}");
        let package = Package::from_registry(&self.client, &url).await?;
        let latest_version = package.get_latest_version()?;

        download_and_extract(
            &package.name,
            &latest_version.npm_version,
            latest_version.get_tarball_url(),
            &self.store_path,
            &self.node_modules_path,
            true,
        )
        .await?;

        if let Some(dependencies) = &latest_version.dependencies {
            join_all(
                dependencies
                    .iter()
                    .map(|(name, version)| self.add_package(name, version))
                    .collect::<Vec<_>>(),
            )
            .await;
        }

        if let Some(dependencies) = &latest_version.dev_dependencies {
            join_all(
                dependencies
                    .iter()
                    .map(|(name, version)| self.add_package(name, version))
                    .collect::<Vec<_>>(),
            )
            .await;
        }

        Ok(())
    }

    async fn add_package(&self, name: &str, version: &str) -> Result<(), RegistryError> {
        let url = format!("https://registry.npmjs.com/{name}");
        let (_version_pin, serialized_version) = parse_version(&version);
        let package = Package::from_registry(&self.client, &url).await?;
        // TODO: Make sure you get the correct version depending on version pin
        let requested_version = package.versions.get(serialized_version).unwrap();

        println!("{}", format!("downloading package {name}@{serialized_version}"));

        download_and_extract(
            &package.name,
            serialized_version,
            requested_version.dist.tarball.as_str(),
            &self.store_path,
            &self.node_modules_path,
            false,
        )
        .await?;
        Ok(())
    }
}
