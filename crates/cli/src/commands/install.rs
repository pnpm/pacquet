use crate::package_manager::{PackageManager, PackageManagerError};
use async_recursion::async_recursion;
use clap::Parser;
use futures_util::future;
use node_semver::Version;
use pacquet_diagnostics::tracing;
use pacquet_lockfile::Lockfile;
use pacquet_package_json::DependencyGroup;
use pacquet_package_manager::{InstallFrozenLockfile, InstallPackageFromRegistry};
use pacquet_registry::PackageVersion;
use pipe_trait::Pipe;

#[derive(Debug, Parser)]
pub struct CliDependencyOptions {
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

impl CliDependencyOptions {
    /// Convert the command arguments to an iterator of [`DependencyGroup`]
    /// which filters the types of dependencies to install.
    fn dependency_groups(&self) -> impl Iterator<Item = DependencyGroup> {
        let &CliDependencyOptions { prod, dev, no_optional } = self;
        let has_both = prod == dev;
        let has_prod = has_both || prod;
        let has_dev = has_both || dev;
        let has_optional = !no_optional;
        std::iter::empty()
            .chain(has_prod.then_some(DependencyGroup::Default))
            .chain(has_dev.then_some(DependencyGroup::Dev))
            .chain(has_optional.then_some(DependencyGroup::Optional))
    }
}

#[derive(Parser, Debug)]
pub struct InstallCommandArgs {
    /// --prod, --dev, and --no-optional
    #[clap(flatten)]
    pub dependency_options: CliDependencyOptions,

    /// Don't generate a lockfile and fail if the lockfile is outdated.
    #[clap(long)]
    pub frozen_lockfile: bool,
}

impl PackageManager {
    /// Install dependencies of a dependency.
    ///
    /// This function is used by [`PackageManager::install`] without a lockfile.
    #[async_recursion]
    async fn install_dependencies_from_registry(&self, package: &PackageVersion) {
        let node_modules_path = self
            .config
            .virtual_store_dir
            .join(package.to_virtual_store_name())
            .join("node_modules");

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Start subset");

        package
            .dependencies(self.config.auto_install_peers)
            .map(|(name, version_range)| async {
                let dependency = InstallPackageFromRegistry {
                    tarball_cache: &self.tarball_cache,
                    http_client: &self.http_client,
                    config: self.config,
                    node_modules_dir: &node_modules_path,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .unwrap(); // TODO: proper error propagation
                self.install_dependencies_from_registry(&dependency).await;
            })
            .pipe(future::join_all)
            .await;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");
    }

    /// Jobs of the `install` command.
    pub async fn install(&self, args: &InstallCommandArgs) -> Result<(), PackageManagerError> {
        let InstallCommandArgs { dependency_options, frozen_lockfile } = args;
        tracing::info!(target: "pacquet::install", "Start all");

        match (self.config.lockfile, frozen_lockfile, &self.lockfile) {
            (false, _, _) => {
                self.package_json
                    .dependencies(dependency_options.dependency_groups())
                    .map(|(name, version_range)| async move {
                        let dependency = InstallPackageFromRegistry {
                            tarball_cache: &self.tarball_cache,
                            http_client: &self.http_client,
                            config: self.config,
                            node_modules_dir: &self.config.modules_dir,
                            name,
                            version_range,
                        }
                        .run::<Version>()
                        .await
                        .unwrap();
                        self.install_dependencies_from_registry(&dependency).await;
                    })
                    .pipe(future::join_all)
                    .await;
            }
            (true, false, Some(_)) | (true, false, None) | (true, true, None) => {
                unimplemented!();
            }
            (true, true, Some(lockfile)) => {
                let Lockfile { lockfile_version, project_snapshot, packages, .. } = lockfile;
                assert_eq!(lockfile_version.major, 6); // compatibility check already happens at serde, but this still helps preventing programmer mistakes.

                let PackageManager { config, http_client, tarball_cache, .. } = self;
                InstallFrozenLockfile {
                    tarball_cache,
                    http_client,
                    config,
                    project_snapshot,
                    packages: packages.as_ref(),
                    dependency_groups: args.dependency_options.dependency_groups(),
                }
                .run()
                .await;
            }
        }

        tracing::info!(target: "pacquet::install", "Complete all");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::Result;

    use crate::commands::install::{CliDependencyOptions, InstallCommandArgs};
    use crate::fs::get_all_folders;
    use crate::package_manager::PackageManager;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    // Helper function to check if a path is a symlink or junction
    fn is_symlink_or_junction(path: std::path::PathBuf) -> Result<bool> {
        #[cfg(windows)]
        return junction::exists(&path);

        #[cfg(not(windows))]
        return Ok(path.is_symlink());
    }

    #[test]
    fn install_args_to_dependency_groups() {
        use DependencyGroup::{Default, Dev, Optional};
        let create_list = |opts: CliDependencyOptions| opts.dependency_groups().collect::<Vec<_>>();

        // no flags -> prod + dev + optional
        assert_eq!(
            create_list(CliDependencyOptions { prod: false, dev: false, no_optional: false }),
            [Default, Dev, Optional],
        );

        // --prod -> prod + optional
        assert_eq!(
            create_list(CliDependencyOptions { prod: true, dev: false, no_optional: false }),
            [Default, Optional],
        );

        // --dev -> dev + optional
        assert_eq!(
            create_list(CliDependencyOptions { prod: false, dev: true, no_optional: false }),
            [Dev, Optional],
        );

        // --no-optional -> prod + dev
        assert_eq!(
            create_list(CliDependencyOptions { prod: false, dev: false, no_optional: true }),
            [Default, Dev],
        );

        // --prod --no-optional -> prod
        assert_eq!(
            create_list(CliDependencyOptions { prod: true, dev: false, no_optional: true }),
            [Default],
        );

        // --dev --no-optional -> dev
        assert_eq!(
            create_list(CliDependencyOptions { prod: false, dev: true, no_optional: true }),
            [Dev],
        );

        // --prod --dev -> prod + dev + optional
        assert_eq!(
            create_list(CliDependencyOptions { prod: true, dev: true, no_optional: false }),
            [Default, Dev, Optional],
        );

        // --prod --dev --no-optional -> prod + dev
        assert_eq!(
            create_list(CliDependencyOptions { prod: true, dev: true, no_optional: true }),
            [Default, Dev],
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

        let package_manager =
            PackageManager::new(&package_json_path, Npmrc::current().leak()).unwrap();
        let args = InstallCommandArgs {
            dependency_options: CliDependencyOptions {
                prod: false,
                dev: false,
                no_optional: false,
            },
            frozen_lockfile: false,
        };
        package_manager.install(&args).await.unwrap();

        // Make sure the package is installed
        assert!(is_symlink_or_junction(dir.path().join("node_modules/is-odd")).unwrap());
        assert!(dir.path().join("node_modules/.pacquet/is-odd@3.0.1").exists());
        // Make sure it installs direct dependencies
        assert!(!dir.path().join("node_modules/is-number").exists());
        assert!(dir.path().join("node_modules/.pacquet/is-number@6.0.0").exists());
        // Make sure we install dev-dependencies as well
        assert!(is_symlink_or_junction(dir.path().join("node_modules/fast-decode-uri-component"))
            .unwrap());
        assert!(dir.path().join("node_modules/.pacquet/fast-decode-uri-component@1.0.1").is_dir());

        insta::assert_debug_snapshot!(get_all_folders(dir.path()));

        env::set_current_dir(&current_directory).unwrap();
    }
}
