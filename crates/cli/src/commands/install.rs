use crate::package_manager::{PackageManager, PackageManagerError};
use futures_util::future::join_all;
use pacquet_package_json::DependencyGroup;

impl PackageManager {
    pub async fn install(
        &self,
        install_dev_dependencies: bool,
        install_optional_dependencies: bool,
    ) -> Result<(), PackageManagerError> {
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

    use crate::package_manager::PackageManager;
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use tempfile::tempdir;

    #[tokio::test]
    pub async fn should_install_dependencies() {
        let dir = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(dir.path()).unwrap();

        let package_json_path = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(&package_json_path).unwrap();

        package_json.add_dependency("is-odd", "3.0.1", DependencyGroup::Default).unwrap();
        package_json
            .add_dependency("fast-decode-uri-component", "1.0.1", DependencyGroup::Dev)
            .unwrap();

        package_json.save().unwrap();

        let package_manager = PackageManager::new(&package_json_path).unwrap();
        package_manager.install(true, false).await.unwrap();
        // Make sure the package is installed
        assert!(dir.path().join("node_modules/is-odd").is_symlink());
        assert!(dir.path().join("node_modules/.pacquet/is-odd@3.0.1").exists());
        // Make sure it installs direct dependencies
        assert!(!dir.path().join("node_modules/is-number").exists());
        assert!(dir.path().join("node_modules/.pacquet/is-number@6.0.0").exists());
        // Make sure we install dev-dependencies as well
        assert!(dir.path().join("node_modules/fast-decode-uri-component").is_symlink());
        assert!(dir.path().join("node_modules/.pacquet/fast-decode-uri-component@1.0.1").is_dir());

        env::set_current_dir(&current_directory).unwrap();
    }
}
