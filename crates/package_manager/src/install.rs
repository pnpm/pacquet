use futures_util::future::join_all;
use pacquet_package_json::DependencyGroup;
use pacquet_registry::RegistryError;

impl crate::PackageManager {
    pub async fn install(
        &mut self,
        install_dev_dependencies: bool,
        install_optional_dependencies: bool,
    ) -> Result<(), RegistryError> {
        let mut dependency_groups = vec![DependencyGroup::Default, DependencyGroup::Optional];
        if install_dev_dependencies {
            dependency_groups.push(DependencyGroup::Dev);
        };
        if !install_optional_dependencies {
            dependency_groups.remove(1);
        }
        let dependencies = self.package_json.get_dependencies(dependency_groups);

        join_all(
            dependencies
                .iter()
                .map(|(name, version)| {
                    self.install_package(name, version, &self.config.modules_dir)
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

    use pacquet_package_json::{DependencyGroup, PackageJson};
    use tempfile::tempdir;

    use crate::PackageManager;

    #[tokio::test]
    pub async fn should_install_dependencies() {
        let dir = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(dir.path()).unwrap();

        let package_json_path = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(&package_json_path).unwrap();

        package_json.add_dependency("is-odd", "3.0.1", DependencyGroup::Default).unwrap();

        package_json.save().unwrap();

        let mut package_manager = PackageManager::new(&package_json_path).unwrap();
        package_manager.install(false, false).await.unwrap();

        assert!(dir.path().join("node_modules/is-odd").is_symlink());

        env::set_current_dir(&current_directory).unwrap();
    }
}
