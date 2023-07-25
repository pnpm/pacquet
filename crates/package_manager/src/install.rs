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
