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

impl InstallCommandArgs {
    /// Convert the command arguments to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    fn get_dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let InstallCommandArgs { prod, dev, no_optional } = self;
        let has_both = !prod && !dev;
        let has_prod = has_both || *prod;
        let has_dev = has_both || *dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Default))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

impl PackageManager {
    pub async fn install(&self, args: &InstallCommandArgs) -> Result<(), PackageManagerError> {
        let config = &self.config;
        let path = &self.config.modules_dir;
        let http_client = &self.http_client;
        let mut queue: VecDeque<Vec<PackageVersion>> = VecDeque::new();

        let direct_dependency_handles = self
            .package_json
            .get_dependencies(args.get_dependency_groups())
            .map(|(name, version)| async move {
                find_package_version_from_registry(config, http_client, name, version, path)
                    .await
                    .unwrap()
            });

        queue.push_front(future::join_all(direct_dependency_handles).await);

        while let Some(dependencies) = queue.pop_front() {
            for dependency in dependencies {
                let node_modules_path = self
                    .config
                    .virtual_store_dir
                    .join(dependency.get_store_name())
                    .join("node_modules");

                let handles = dependency.get_dependencies(self.config.auto_install_peers).map(
                    |(name, version)| async {
                        find_package_version_from_registry(
                            config,
                            http_client,
                            name,
                            version,
                            &node_modules_path,
                        )
                        .await
                        .unwrap()
                    },
                );

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

    #[test]
    fn install_args_to_dependency_groups() {
        use DependencyGroup::{Default, Dev, Optional};
        let create_list =
            |args: InstallCommandArgs| args.get_dependency_groups().collect::<Vec<_>>();

        // no flags -> prod + dev + optional
        assert_eq!(
            create_list(InstallCommandArgs { prod: false, dev: false, no_optional: false }),
            [Default, Dev, Optional],
        );

        // --prod -> prod + optional
        assert_eq!(
            create_list(InstallCommandArgs { prod: true, dev: false, no_optional: false }),
            [Default, Optional],
        );

        // --dev -> dev + optional
        assert_eq!(
            create_list(InstallCommandArgs { prod: false, dev: true, no_optional: false }),
            [Dev, Optional],
        );

        // --no-optional -> prod + dev
        assert_eq!(
            create_list(InstallCommandArgs { prod: false, dev: false, no_optional: true }),
            [Default, Dev],
        );

        // --prod --no-optional -> prod
        assert_eq!(
            create_list(InstallCommandArgs { prod: true, dev: false, no_optional: true }),
            [Default],
        );

        // --dev --no-optional -> dev
        assert_eq!(
            create_list(InstallCommandArgs { prod: false, dev: true, no_optional: true }),
            [Dev],
        );
    }

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
