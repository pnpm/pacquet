use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::commands::AddArgs;
use crate::package_import::import_packages_to_virtual_dir;
use crate::package_manager::{PackageManager, PackageManagerError};
use futures_util::future;
use pacquet_npmrc::Npmrc;
use pacquet_registry::get_package_from_registry;
use pacquet_registry::package_version::PackageVersion;
use pacquet_tarball::download_tarball_to_store;

impl PackageManager {
    /// Here is a brief overview of what this package does.
    /// 1. Get a dependency
    /// 2. Save the dependency to node_modules/.pacquet/pkg@version/node_modules/pkg
    /// 3. Create a symlink to node_modules/pkg
    /// 4. Download all dependencies to node_modules/.pacquet
    /// 5. Symlink all dependencies to node_modules/.pacquet/pkg@version/node_modules
    /// 6. Update package.json
    pub async fn add(&mut self, args: &AddArgs) -> Result<(), PackageManagerError> {
        let package_version =
            get_package_from_registry(&args.package, &self.http_client, &self.config.registry)
                .await?;
        let latest_version = package_version.get_latest()?;
        let store_name = latest_version.get_store_name();
        let package_node_modules_path =
            self.config.virtual_store_dir.join(store_name).join("node_modules");

        {
            // Install, extract and symlink tarball to necessary locations.
            let cas_paths = download_tarball_to_store(
                &self.http_client,
                &self.config.store_dir,
                &latest_version,
                latest_version.get_tarball_url(),
            )
            .await?;

            import_packages_to_virtual_dir(
                &self.config.package_import_method,
                &cas_paths,
                &package_node_modules_path.join(&args.package),
                &self.config.modules_dir.join(&args.package),
            )
            .await?;
        }

        let mut queue: VecDeque<(PathBuf, Vec<PackageVersion>)> = VecDeque::new();
        let config = &self.config;
        let http_client = &self.http_client;

        let handles = latest_version
            .get_dependencies(self.config.auto_install_peers)
            .iter()
            .map(|(name, version)| async {
                let path = &package_node_modules_path;
                fetch_package(config, http_client, name, version, path).await.unwrap()
            })
            .collect::<Vec<_>>();

        let results = future::join_all(handles).await;

        queue.push_front((package_node_modules_path, results));

        while let Some((symlink_to_folder, dependencies)) = queue.pop_front() {
            for dependency in dependencies {
                let node_modules_path = self
                    .config
                    .virtual_store_dir
                    .join(dependency.get_store_name())
                    .join("node_modules");

                println!(
                    "package-> {}, node_modules_path -> {}",
                    dependency.name,
                    node_modules_path.display()
                );

                let handles = dependency
                    .get_dependencies(self.config.auto_install_peers)
                    .iter()
                    .map(|(name, version)| async {
                        fetch_package(config, http_client, name, version, &symlink_to_folder)
                            .await
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                queue.push_back((node_modules_path, future::join_all(handles).await));
            }
        }

        self.package_json.add_dependency(
            &args.package,
            &latest_version.serialize(args.save_exact),
            args.get_dependency_group(),
        )?;
        self.package_json.save()?;

        Ok(())
    }
}

pub async fn fetch_package(
    config: &Npmrc,
    http_client: &reqwest::Client,
    name: &str,
    version: &str,
    symlink_path: &Path,
) -> Result<PackageVersion, PackageManagerError> {
    let package = get_package_from_registry(name, http_client, &config.registry).await?;
    let package_version = package.get_suitable_version_of(version)?.unwrap();
    let dependency_store_folder_name = package_version.get_store_name();
    let package_node_modules_path =
        config.virtual_store_dir.join(dependency_store_folder_name).join("node_modules");

    let cas_paths = download_tarball_to_store(
        http_client,
        &config.store_dir,
        &package_version,
        package_version.get_tarball_url(),
    )
    .await?;

    import_packages_to_virtual_dir(
        &config.package_import_method,
        &cas_paths,
        &package_node_modules_path.join(name),
        &symlink_path.join(&package.name),
    )
    .await?;

    Ok(package_version.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use crate::fs::get_filenames_in_folder;
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    pub async fn should_install_all_dependencies() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json).unwrap();

        // It should create a package_json if not exist
        assert!(package_json.exists());

        let args = AddArgs {
            package: "is-even".to_string(),
            save_prod: false,
            save_dev: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();
        assert!(dir.path().join("node_modules/is-even").is_symlink());
        assert!(dir.path().join("node_modules/is-even").join("package.json").exists());

        // Check if all dependencies are loaded.
        let dependencies = [
            "is-buffer@1.1.6",
            "is-even@1.0.0",
            "is-number@3.0.0",
            "is-odd@0.1.2",
            "kind-of@3.2.2",
        ];
        dependencies.iter().for_each(|dep| {
            assert!(virtual_store_dir.join(dep).is_dir());
        });

        // Ensure that is-buffer does not have any dependencies
        let is_buffer_path = virtual_store_dir.join("is-buffer@1.1.6/node_modules");
        assert_eq!(get_filenames_in_folder(&is_buffer_path), vec!["is-buffer"]);

        // Ensure that is-even have correct dependencies
        let is_even_path = virtual_store_dir.join("is-even@1.0.0/node_modules");
        assert_eq!(get_filenames_in_folder(&is_even_path), vec!["is-even", "is-odd"]);

        // Ensure that is-number does not have any dependencies
        let is_number_path = virtual_store_dir.join("is-number@3.0.0/node_modules");
        assert_eq!(get_filenames_in_folder(&is_number_path), vec!["is-number"]);

        env::set_current_dir(&current_directory).unwrap();
    }

    #[tokio::test]
    pub async fn should_symlink_correctly() {
        let dir = tempdir().unwrap();
        let virtual_store_dir = dir.path().join("node_modules/.pacquet");
        let current_directory = env::current_dir().unwrap();
        env::set_current_dir(&dir).unwrap();
        let package_json = dir.path().join("package.json");
        let mut manager = PackageManager::new(&package_json).unwrap();

        let args = AddArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();

        // Make sure the symlinks are correct
        assert_eq!(
            fs::read_link(virtual_store_dir.join("is-odd@0.1.2/node_modules/is-number")).unwrap(),
            fs::canonicalize(virtual_store_dir.join("is-number@3.0.0/node_modules/is-number"))
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
        let mut manager = PackageManager::new(&package_json).unwrap();

        let args = AddArgs {
            package: "is-odd".to_string(),
            save_prod: false,
            save_dev: false,
            save_optional: false,
            save_exact: false,
            virtual_store_dir: virtual_store_dir.to_string_lossy().to_string(),
        };
        manager.add(&args).await.unwrap();
        let file = PackageJson::from_path(&dir.path().join("package.json")).unwrap();
        assert!(file.get_dependencies(vec![DependencyGroup::Default]).contains_key("is-odd"));
        env::set_current_dir(&current_directory).unwrap();
    }
}
