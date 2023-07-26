use std::path::Path;

use async_recursion::async_recursion;
use futures_util::future::join_all;
use pacquet_package_json::DependencyGroup;
use pacquet_registry::RegistryError;
use pacquet_tarball::get_package_store_folder_name;

use crate::PackageManagerError;

impl crate::PackageManager {
    /// Here is a brief overview of what this package does.
    /// 1. Get a dependency
    /// 2. Save the dependency to node_modules/.pacquet/pkg@version/node_modules/pkg
    /// 3. Create a symlink to node_modules/pkg
    /// 4. Download all dependencies to node_modules/.pacquet
    /// 5. Symlink all dependencies to node_modules/.pacquet/pkg@version/node_modules
    /// 6. Update package.json
    pub async fn add(
        &mut self,
        name: &str,
        dependency_group: DependencyGroup,
        save_exact: bool,
    ) -> Result<(), PackageManagerError> {
        let latest_version = self.registry.get_package_by_version(name, "latest").await?;
        let dependency_store_folder_name =
            get_package_store_folder_name(name, &latest_version.version.to_string());

        let package_node_modules_path =
            self.config.virtual_store_dir.join(dependency_store_folder_name).join("node_modules");

        self.tarball
            .download_dependency(
                &latest_version.dist.integrity,
                latest_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &self.config.modules_dir.join(name),
            )
            .await?;

        join_all(
            latest_version
                .get_dependencies(self.config.auto_install_peers)
                .iter()
                .map(|(name, version)| {
                    self.install_package(name, version, &package_node_modules_path)
                })
                .collect::<Vec<_>>(),
        )
        .await;

        self.package_json.add_dependency(
            name,
            &latest_version.serialize(save_exact),
            dependency_group,
        )?;
        self.package_json.save()?;

        Ok(())
    }

    #[async_recursion(?Send)]
    pub async fn install_package(
        &self,
        name: &str,
        version: &str,
        symlink_path: &Path,
    ) -> Result<(), RegistryError> {
        let package = self.registry.get_package(name).await?;
        let package_version = package.get_suitable_version_of(version)?.unwrap();
        let dependency_store_folder_name =
            get_package_store_folder_name(name, &package_version.version.to_string());
        let package_node_modules_path =
            self.config.virtual_store_dir.join(dependency_store_folder_name).join("node_modules");

        // Make sure to lock the package's mutex so we don't install the same package's tarball
        // in different threads.
        let mutex_guard = package.mutex.lock().await;

        self.tarball
            .download_dependency(
                &package_version.dist.integrity,
                package_version.get_tarball_url(),
                &package_node_modules_path.join(name),
                &symlink_path.join(&package.name),
            )
            .await?;

        drop(mutex_guard);

        join_all(
            package_version
                .get_dependencies(self.config.auto_install_peers)
                .iter()
                .map(|(name, version)| {
                    self.install_package(name, version, &package_node_modules_path)
                })
                .collect::<Vec<_>>(),
        )
        .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use pacquet_package_json::PackageJson;
    use tempfile::tempdir;

    use super::*;
    use crate::PackageManager;

    #[tokio::test]
    pub async fn should_add_a_package_with_no_dependencies() {
        let dir = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json).unwrap();

        // It should create a package_json if not exist
        assert!(package_json.exists());

        manager.add("is-odd", DependencyGroup::Default, false).await.unwrap();

        let package_path = dir.path().join("node_modules/is-odd");
        assert!(package_path.exists());
        assert!(package_path.is_symlink());
        assert!(package_path.join("package.json").exists());

        let file = PackageJson::from_path(&dir.path().join("package.json")).unwrap();
        assert!(file.get_dependencies(vec![DependencyGroup::Default]).contains_key("is-odd"));

        env::set_current_dir(&current_directory).unwrap();
    }
}
