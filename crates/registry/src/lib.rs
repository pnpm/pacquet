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
use pacquet_tarball::{download_direct_dependency, download_indirect_dependency};

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
        let id = format!("{name}@{0}", latest_version.version);

        download_direct_dependency(
            &package.name,
            &latest_version.version.to_string(),
            latest_version.get_tarball_url(),
            &self.node_modules_path,
            &self.store_path,
            &id,
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

        let package_node_modules_path = self.store_path.join(id).join("node_modules");
        join_all(
            all_dependencies
                .into_iter()
                .map(|(name, version)| self.add_package(name, version, &package_node_modules_path))
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
        name_field: &str,
        version_field: &str,
        symlink_path: &Path,
    ) -> Result<(), RegistryError> {
        let package = self.client.get_package(name_field).await?;
        let package_version = package.get_suitable_version_of(version_field)?.unwrap();

        download_indirect_dependency(
            &package.name,
            &package_version.version.to_string(),
            package_version.get_tarball_url(),
            &self.store_path,
            &symlink_path.join(&package.name),
        )
        .await?;

        let all_dependencies: HashMap<String, String> =
            package_version.dependencies.clone().unwrap_or(HashMap::<String, String>::new());

        let store_folder_name =
            format!("{0}@{1}", name_field.replace('/', "+"), package_version.version);
        let package_node_modules_path =
            self.store_path.join(store_folder_name).join("node_modules");
        join_all(
            all_dependencies
                .iter()
                .map(|(name, version)| self.add_package(name, version, &package_node_modules_path))
                .collect::<Vec<_>>(),
        )
        .await;

        Ok(())
    }
}
