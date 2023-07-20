mod error;
mod http_client;
mod package;

use std::path::{Path, PathBuf};

use async_recursion::async_recursion;
use futures_util::future::join_all;
use pacquet_npmrc::{get_current_npmrc, Npmrc};
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_tarball::{get_package_store_folder_name, TarballManager};

use crate::{error::RegistryError, http_client::HttpClient};

pub struct RegistryManager {
    client: Box<HttpClient>,
    config: Box<Npmrc>,
    package_json: Box<PackageJson>,
    tarball_manager: Box<TarballManager>,
}

impl RegistryManager {
    pub fn new<P: Into<PathBuf>>(package_json_path: P) -> Result<RegistryManager, RegistryError> {
        let config = get_current_npmrc();
        Ok(RegistryManager {
            client: Box::new(HttpClient::new(&config.registry)),
            config: Box::new(config),
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
            self.config.virtual_store_dir.join(dependency_store_folder_name).join("node_modules");

        self.tarball_manager
            .download_dependency(
                name,
                latest_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &self.config.modules_dir.join(name),
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
            self.config.virtual_store_dir.join(dependency_store_folder_name).join("node_modules");

        // Make sure to lock the package's mutex so we don't install the same package's tarball
        // in different threads.
        let mutex_guard = package.mutex.lock().await;

        self.tarball_manager
            .download_dependency(
                name,
                package_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &symlink_path.join(&package.name),
            )
            .await?;

        drop(mutex_guard);

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
