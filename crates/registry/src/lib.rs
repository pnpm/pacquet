mod error;
mod http_client;
mod package;

use std::path::{Path, PathBuf};

use async_recursion::async_recursion;
use futures_util::future::join_all;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_tarball::{get_package_store_folder_name, TarballManager};

use crate::{error::RegistryError, http_client::HttpClient};

pub struct RegistryManager {
    client: Box<HttpClient>,
    node_modules_path: PathBuf,
    store_path: PathBuf,
    package_json: Box<PackageJson>,
    tarball_manager: Box<TarballManager>,
}

impl RegistryManager {
    pub fn new<P: Into<PathBuf>>(
        node_modules_path: P,
        store_path: P,
        package_json_path: P,
    ) -> Result<RegistryManager, RegistryError> {
        Ok(RegistryManager {
            client: Box::new(HttpClient::new()),
            node_modules_path: node_modules_path.into(),
            store_path: store_path.into(),
            package_json: Box::new(PackageJson::create_if_needed(&package_json_path.into())?),
            tarball_manager: Box::new(TarballManager::new()),
        })
    }

    /// Here is a brief overview of what this package does.
    /// 1. Get a dependency
    /// 2. Save the dependency to node_modules/.pacquet/pkg@version/node_modules/pkg
    /// 3. Create a symlink to node_modules/pkg
    /// 4. Download all dependencies to node_modules/.pacquet
    /// 5. Symlink all dependencies to node_modules/.pacquet/pkg@version/node_modules
    /// 6. Update package.json
    pub async fn add_dependency(
        &mut self,
        name: &str,
        dependency_group: DependencyGroup,
    ) -> Result<(), RegistryError> {
        let latest_version = self.client.get_package_by_version(name, "latest").await?;
        let dependency_store_folder_name =
            get_package_store_folder_name(name, &latest_version.version.to_string());

        let package_node_modules_path =
            self.store_path.join(dependency_store_folder_name).join("node_modules");

        self.tarball_manager
            .download_dependency(
                name,
                latest_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &self.node_modules_path.join(name),
            )
            .await?;

        if let Some(dependencies) = latest_version.dependencies.as_ref() {
            join_all(
                dependencies
                    .iter()
                    .map(|(name, version)| {
                        self.add_package(name, version, &package_node_modules_path)
                    })
                    .collect::<Vec<_>>(),
            )
            .await;
        }

        self.package_json.add_dependency(
            name,
            &format!("^{0}", &latest_version.version),
            dependency_group,
        )?;
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
        let package_node_modules_path =
            self.store_path.join(dependency_store_folder_name).join("node_modules");

        // TODO: We shouldn't call this function for multiple same packages.
        // There needs to be some sort of thread safety.
        self.tarball_manager
            .download_dependency(
                name,
                package_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &symlink_path.join(&package.name),
            )
            .await?;

        if let Some(dependencies) = package_version.dependencies.as_ref() {
            join_all(
                dependencies
                    .iter()
                    .map(|(name, version)| {
                        self.add_package(name, version, &package_node_modules_path)
                    })
                    .collect::<Vec<_>>(),
            )
            .await;
        }

        Ok(())
    }
}
