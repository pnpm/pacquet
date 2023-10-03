use clap::Parser;
use std::collections::VecDeque;

use crate::{
    package::{fetch_package_version_directly, install_package_from_registry},
    package_manager::{PackageManager, PackageManagerError},
};
use futures_util::future;
use pacquet_diagnostics::miette::WrapErr;
use pacquet_package_json::DependencyGroup;
use pacquet_registry::PackageVersion;

#[derive(Parser, Debug)]
pub struct AddCommandArgs {
    /// Name of the package
    pub package: String,
    /// Install the specified packages as regular dependencies.
    #[arg(short = 'P', long = "save-prod", group = "dependency_group")]
    save_prod: bool,
    /// Install the specified packages as devDependencies.
    #[arg(short = 'D', long = "save-dev", group = "dependency_group")]
    save_dev: bool,
    /// Install the specified packages as optionalDependencies.
    #[arg(short = 'O', long = "save-optional", group = "dependency_group")]
    save_optional: bool,
    /// Using --save-peer will add one or more packages to peerDependencies and install them as dev dependencies
    #[arg(long = "save-peer", group = "dependency_group")]
    save_peer: bool,
    /// Saved dependencies will be configured with an exact version rather than using
    /// pacquet's default semver range operator.
    #[arg(short = 'E', long = "save-exact")]
    pub save_exact: bool,
    /// The directory with links to the store (default is node_modules/.pacquet).
    /// All direct and indirect dependencies of the project are linked into this directory
    #[arg(long = "virtual-store-dir", default_value = "node_modules/.pacquet")]
    pub virtual_store_dir: String,
}

impl AddCommandArgs {
    pub fn dependency_group(&self) -> DependencyGroup {
        if self.save_dev {
            DependencyGroup::Dev
        } else if self.save_optional {
            DependencyGroup::Optional
        } else if self.save_peer {
            DependencyGroup::Peer
        } else {
            DependencyGroup::Default
        }
    }
}

impl PackageManager {
    /// Here is a brief overview of what this package does.
    /// 1. Get a dependency
    /// 2. Save the dependency to node_modules/.pacquet/pkg@version/node_modules/pkg
    /// 3. Create a symlink to node_modules/pkg
    /// 4. Download all dependencies to node_modules/.pacquet
    /// 5. Symlink all dependencies to node_modules/.pacquet/pkg@version/node_modules
    /// 6. Update package.json
    pub async fn add(&mut self, args: &AddCommandArgs) -> Result<(), PackageManagerError> {
        let latest_version = fetch_package_version_directly(
            &self.tarball_cache,
            self.config,
            &self.http_client,
            &args.package,
            "latest",
            &self.config.modules_dir,
        )
        .await?;
        let package_node_modules_path = self
            .config
            .virtual_store_dir
            .join(latest_version.to_virtual_store_name())
            .join("node_modules");

        let mut queue: VecDeque<Vec<Result<PackageVersion, PackageManagerError>>> = VecDeque::new();
        let config = &self.config;
        let http_client = &self.http_client;
        let path = &package_node_modules_path;

        let direct_dependency_handles =
            latest_version.dependencies(self.config.auto_install_peers).map(|(name, version)| {
                install_package_from_registry(
                    &self.tarball_cache,
                    config,
                    http_client,
                    name,
                    version,
                    path,
                )
            });

        queue.push_front(future::join_all(direct_dependency_handles).await);

        while let Some(dependencies) = queue.pop_front() {
            for dependency in dependencies {
                let dependency =
                    dependency.wrap_err("failed to install one of the dependencies.").unwrap();
                let node_modules_path = self
                    .config
                    .virtual_store_dir
                    .join(dependency.to_virtual_store_name())
                    .join("node_modules");

                let handles = dependency.dependencies(self.config.auto_install_peers).map(
                    |(name, version)| {
                        install_package_from_registry(
                            &self.tarball_cache,
                            config,
                            http_client,
                            name,
                            version,
                            &node_modules_path,
                        )
                    },
                );

                queue.push_back(future::join_all(handles).await);
            }
        }

        self.package_json.add_dependency(
            &args.package,
            &latest_version.serialize(args.save_exact),
            args.dependency_group(),
        )?;
        // Using --save-peer will add one or more packages to peerDependencies and
        // install them as dev dependencies
        if args.dependency_group() == DependencyGroup::Peer {
            self.package_json.add_dependency(
                &args.package,
                &latest_version.serialize(args.save_exact),
                DependencyGroup::Dev,
            )?;
        }
        self.package_json.save()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::fs::get_all_folders;
    use std::{env, fs};

    use crate::fs::get_filenames_in_folder;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    pub async fn should_install_all_dependencies() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json, Npmrc::current().leak()).unwrap();

        // It should create a package_json if not exist
        assert!(package_json.exists());

        let args = AddCommandArgs {
            package: "is-even".to_string(),
            save_prod: false,
            save_dev: false,
            save_peer: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();

        insta::assert_debug_snapshot!(get_all_folders(dir.path()));

        // Ensure that is-buffer does not have any dependencies
        let is_buffer_path = virtual_store_dir.join("is-buffer@1.1.6/node_modules");
        assert_eq!(get_filenames_in_folder(&is_buffer_path), vec!["is-buffer"]);

        // Ensure that is-even have correct dependencies
        let is_even_path = virtual_store_dir.join("is-even@1.0.0/node_modules");
        assert_eq!(get_filenames_in_folder(&is_even_path), vec!["is-even", "is-odd"]);

        // Ensure that is-number does not have any dependencies
        let is_number_path = virtual_store_dir.join("is-number@3.0.0/node_modules");
        assert_eq!(get_filenames_in_folder(&is_number_path), vec!["is-number", "kind-of"]);

        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    pub async fn should_symlink_correctly() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json, Npmrc::current().leak()).unwrap();

        let args = AddCommandArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: false,
            save_peer: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();

        insta::assert_debug_snapshot!(get_all_folders(dir.path()));

        // Make sure the symlinks are correct
        assert_eq!(
            fs::read_link(virtual_store_dir.join("is-odd@3.0.1/node_modules/is-number")).unwrap(),
            fs::canonicalize(virtual_store_dir.join("is-number@6.0.0/node_modules/is-number"))
                .unwrap(),
        );
        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    pub async fn should_add_to_package_json() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json, Npmrc::current().leak()).unwrap();

        let args = AddCommandArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: false,
            save_peer: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();
        let file = PackageJson::from_path(dir.path().join("package.json")).unwrap();
        assert!(file.dependencies([DependencyGroup::Default]).any(|(k, _)| k == "is-odd"));
        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    pub async fn should_add_dev_dependency() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json, Npmrc::current().leak()).unwrap();

        let args = AddCommandArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: true,
            save_peer: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();
        let file = PackageJson::from_path(dir.path().join("package.json")).unwrap();
        assert!(file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "is-odd"));
        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    pub async fn should_add_peer_dependency() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json, Npmrc::current().leak()).unwrap();

        let args = AddCommandArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: false,
            save_peer: true,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();
        let file = PackageJson::from_path(dir.path().join("package.json")).unwrap();
        assert!(file.dependencies([DependencyGroup::Dev]).any(|(k, _)| k == "is-odd"));
        assert!(file.dependencies([DependencyGroup::Peer]).any(|(k, _)| k == "is-odd"));
        env::set_current_dir(&current_directory).unwrap();
    }
}
