use crate::package::find_package_version_from_registry;
use crate::package_manager::{PackageManager, PackageManagerError};
use clap::Parser;
use futures_util::future;
use pacquet_package_json::DependencyGroup;
use pacquet_registry::PackageVersion;
use std::collections::VecDeque;

#[derive(Parser, Debug)]
pub struct InstallCommandArgs {
    /// pacquet will not install any package listed in devDependencies and will remove those insofar
    /// they were already installed, if the NODE_ENV environment variable is set to production.
    /// Use this flag to instruct pacquet to ignore NODE_ENV and take its production status from this
    /// flag instead.
    #[arg(short = 'P', long = "prod")]
    pub prod: bool,
    /// Only devDependencies are installed and dependencies are removed insofar they were
    /// already installed, regardless of the NODE_ENV.
    #[arg(short = 'D', long = "dev")]
    pub dev: bool,
    /// optionalDependencies are not installed.
    #[arg(long = "no-optional")]
    pub no_optional: bool,
}

impl PackageManager {
    pub async fn install(&self, args: &InstallCommandArgs) -> Result<(), PackageManagerError> {
        let mut dependency_groups = vec![DependencyGroup::Default, DependencyGroup::Optional];
        if args.dev {
            dependency_groups.push(DependencyGroup::Dev);
        }
        if !args.no_optional {
            dependency_groups.remove(1);
        }

        let config = &self.config;
        let path = &self.config.modules_dir;
        let http_client = &self.http_client;
        let mut queue: VecDeque<Vec<PackageVersion>> = VecDeque::new(); // QUESTION: is this queue necessary since it only has one element?

        let direct_dependency_handles = self
            .package_json
            .get_dependencies(&dependency_groups)
            .map(|(name, version)| async move {
                find_package_version_from_registry(config, http_client, name, version, path)
                    .await
                    .unwrap()
            })
            .collect::<Vec<_>>();

        queue.push_front(future::join_all(direct_dependency_handles).await);

        while let Some(dependencies) = queue.pop_front() {
            for dependency in dependencies {
                let node_modules_path = self
                    .config
                    .virtual_store_dir
                    .join(dependency.get_store_name())
                    .join("node_modules");

                let handles = dependency
                    .get_dependencies(self.config.auto_install_peers)
                    .map(|(name, version)| async {
                        find_package_version_from_registry(
                            config,
                            http_client,
                            name,
                            version,
                            &node_modules_path,
                        )
                        .await
                        .unwrap()
                    })
                    .collect::<Vec<_>>();

                queue.push_back(future::join_all(handles).await);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use crate::commands::install::InstallCommandArgs;
    use crate::fs::get_all_folders;
    use crate::package_manager::PackageManager;
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use tempfile::tempdir;

    #[tokio::test]
    pub async fn should_install_dependencies() {
        let dir = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(dir.path()).unwrap();

        let package_json_path = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(package_json_path.clone()).unwrap();

        package_json.add_dependency("is-odd", "3.0.1", DependencyGroup::Default).unwrap();
        package_json
            .add_dependency("fast-decode-uri-component", "1.0.1", DependencyGroup::Dev)
            .unwrap();

        package_json.save().unwrap();

        let package_manager = PackageManager::new(&package_json_path).unwrap();
        let args = InstallCommandArgs { prod: false, dev: true, no_optional: false };
        package_manager.install(&args).await.unwrap();

        // Make sure the package is installed
        assert!(dir.path().join("node_modules/is-odd").is_symlink());
        assert!(dir.path().join("node_modules/.pacquet/is-odd@3.0.1").exists());
        // Make sure it installs direct dependencies
        assert!(!dir.path().join("node_modules/is-number").exists());
        assert!(dir.path().join("node_modules/.pacquet/is-number@6.0.0").exists());
        // Make sure we install dev-dependencies as well
        assert!(dir.path().join("node_modules/fast-decode-uri-component").is_symlink());
        assert!(dir.path().join("node_modules/.pacquet/fast-decode-uri-component@1.0.1").is_dir());

        insta::assert_debug_snapshot!(get_all_folders(dir.path()));

        env::set_current_dir(&current_directory).unwrap();
    }
}
