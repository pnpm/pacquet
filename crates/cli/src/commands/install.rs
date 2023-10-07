use crate::package::{install_package_from_registry, install_single_package_to_virtual_store};
use crate::package_import::symlink_pkg;
use crate::package_manager::{PackageManager, PackageManagerError};
use async_recursion::async_recursion;
use clap::Parser;
use futures_util::future;
use pacquet_diagnostics::tracing;
use pacquet_lockfile::{
    DependencyPath, Lockfile, PackageSnapshot, PkgName, PkgNameVerPeer, RootProjectSnapshot,
};
use pacquet_package_json::DependencyGroup;
use pacquet_registry::PackageVersion;
use pipe_trait::Pipe;
use rayon::prelude::*;
use std::collections::HashMap;

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
                let dependency = install_package_from_registry(
                    &self.tarball_cache,
                    self.config,
                    &self.http_client,
                    name,
                    version_range,
                    &node_modules_path,
                )
                .await
                .unwrap();
                self.install_dependencies_from_registry(&dependency).await;
            })
            .pipe(future::join_all)
            .await;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");
    }

    /// Generate filesystem layout for the virtual store at `node_modules/.pacquet`.
    async fn create_virtual_store(
        &self,
        packages: &Option<HashMap<DependencyPath, PackageSnapshot>>,
    ) {
        let Some(packages) = packages else {
            todo!("check project_snapshot, error if it's not empty, do nothing if empty");
        };
        packages
            .iter()
            .map(|(dependency_path, package_snapshot)| async move {
                install_single_package_to_virtual_store(
                    &self.tarball_cache,
                    &self.http_client,
                    self.config,
                    dependency_path,
                    package_snapshot,
                )
                .await
                .unwrap();
            })
            .pipe(future::join_all)
            .await;
    }

    /// Create symlinks for the direct dependencies.
    ///
    /// If package `foo@x.y.z` is declared as a dependency in `package.json`,
    /// symlink `foo -> .pacquet/foo@x.y.z/node_modules/foo` shall be created
    /// in the `node_modules` directory.
    fn link_direct_dependencies(
        &self,
        project_snapshot: &RootProjectSnapshot,
        args: &InstallCommandArgs,
    ) {
        let InstallCommandArgs { dependency_options, .. } = args;

        let RootProjectSnapshot::Single(project_snapshot) = project_snapshot else {
            panic!("Monorepo is not yet supported");
        };

        project_snapshot
            .dependencies_by_groups(dependency_options.dependency_groups())
            .collect::<Vec<_>>()
            .par_iter()
            .for_each(|(name, spec)| {
                // TODO: the code below is not optimal
                let virtual_store_name =
                    PkgNameVerPeer::new(PkgName::clone(name), spec.version.clone())
                        .to_virtual_store_name();

                let name_str = name.to_string();
                symlink_pkg(
                    &self
                        .config
                        .virtual_store_dir
                        .join(virtual_store_name)
                        .join("node_modules")
                        .join(&name_str),
                    &self.config.modules_dir.join(&name_str),
                );
            });
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
                        let dependency = install_package_from_registry(
                            &self.tarball_cache,
                            self.config,
                            &self.http_client,
                            name,
                            version_range,
                            &self.config.modules_dir,
                        )
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

                // TODO: check if the lockfile is out-of-date

                assert!(
                    self.config.prefer_frozen_lockfile,
                    "Non frozen lockfile is not yet supported",
                );

                self.create_virtual_store(packages).await;
                self.link_direct_dependencies(project_snapshot, args);
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
