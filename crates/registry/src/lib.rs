mod error;
mod http_client;
mod package;
mod package_name;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use async_recursion::async_recursion;
use futures_util::future::join_all;
use pacquet_package_json::PackageJson;
use pacquet_tarball::{download_dependency, get_package_store_folder_name};

use crate::{error::RegistryError, http_client::HttpClient};

pub struct RegistryManager {
    client: HttpClient,
    node_modules_path: PathBuf,
    store_path: PathBuf,
    package_json: PackageJson,
}

impl RegistryManager {
    pub fn new<P: Into<PathBuf>>(
        node_modules_path: P,
        store_path: P,
        package_json_path: P,
    ) -> Result<RegistryManager, RegistryError> {
        Ok(RegistryManager {
            client: HttpClient::new(),
            node_modules_path: node_modules_path.into(),
            store_path: store_path.into(),
            package_json: PackageJson::create_if_needed(&package_json_path.into())?,
        })
    }

    pub fn prepare(&self) -> Result<(), RegistryError> {
        // create store path.
        fs::create_dir_all(&self.store_path)?;
        Ok(())
    }

    pub async fn add_dependency(&mut self, name: &str) -> Result<(), RegistryError> {
        let package = self.client.get_package(name).await?;
        let latest_version = package.get_latest_version()?;
        let dependency_store_folder_name =
            get_package_store_folder_name(name, &latest_version.version.to_string());

        let save_path =
            self.store_path.join(dependency_store_folder_name).join("node_modules").join(name);
        let symlink_to = self.node_modules_path.join(name);

        download_dependency(
            name,
            latest_version.get_tarball_url(),
            save_path.as_ref(),
            symlink_to.as_ref(),
        )
        .await?;

        let mut all_dependencies: HashMap<&String, &String> = HashMap::new();

        if let Some(deps) = latest_version.dependencies.as_ref() {
            all_dependencies.extend(deps);
        }

        // TODO: Enable installing dev_dependencies as well.
        // if let Some(dev_dependencies) = &latest_version.dev_dependencies {
        //     all_dependencies.extend(dev_dependencies);
        // }

        join_all(
            all_dependencies
                .into_iter()
                .map(|(name, version)| self.add_package(name, version, save_path.parent().unwrap()))
                .collect::<Vec<_>>(),
        )
        .await;

        self.package_json.add_dependency(name, &format!("^{0}", &latest_version.version))?;
        self.package_json.save()?;

        Ok(())
    }

    #[async_recursion(?Send)]
    async fn add_package(
        &self,
        name: &str,
        version: &str,
        symlink_path: &Path,
    ) -> Result<(), RegistryError> {
        let package = self.client.get_package(name).await?;
        let package_version = package.get_suitable_version_of(version)?.unwrap();
        let dependency_store_folder_name =
            get_package_store_folder_name(name, &package_version.version.to_string());
        let save_path =
            self.store_path.join(dependency_store_folder_name).join("node_modules").join(name);

        download_dependency(
            name,
            package_version.get_tarball_url(),
            save_path.as_ref(),
            &symlink_path.join(&package.name),
        )
        .await?;

        let all_dependencies: HashMap<String, String> =
            package_version.dependencies.clone().unwrap_or(HashMap::<String, String>::new());

        let mut symlink_path = save_path.parent().unwrap();

        // If package is under an organization such as @fastify/error
        // We need to go 2 folders to find the correct node_modules folder.
        // For example symlink_path should be node_modules for node_modules/@fastify/error.
        if name.contains('/') {
            symlink_path = symlink_path.parent().unwrap();
        }

        join_all(
            all_dependencies
                .iter()
                .map(|(name, version)| self.add_package(name, version, symlink_path))
                .collect::<Vec<_>>(),
        )
        .await;

        Ok(())
    }
}
