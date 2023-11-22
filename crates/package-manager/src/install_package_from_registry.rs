use crate::{create_cas_files, symlink_package, CreateCasFilesError, SymlinkPackageError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::IoThread;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry::{Package, PackageTag, PackageVersion, RegistryError};
use pacquet_tarball::{DownloadTarballToStore, MemCache, TarballError};
use std::{path::Path, str::FromStr};

/// This subroutine executes the following and returns the package
/// * Retrieves the package from the registry
/// * Extracts the tarball to global store directory (~/Library/../pacquet)
/// * Links global store directory to virtual dir (node_modules/.pacquet/..)
///
/// `symlink_path` will be appended by the name of the package. Therefore,
/// it should be resolved into the node_modules folder of a subdependency such as
/// `node_modules/.pacquet/fastify@1.0.0/node_modules`.
#[must_use]
pub struct InstallPackageFromRegistry<'a> {
    pub tarball_mem_cache: &'a MemCache,
    pub http_client: &'a ThrottledClient,
    pub io_thread: &'a IoThread,
    pub config: &'static Npmrc,
    pub node_modules_dir: &'a Path,
    pub name: &'a str,
    pub version_range: &'a str,
}

/// Error type of [`InstallPackageFromRegistry`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageFromRegistryError {
    FetchFromRegistry(#[error(source)] RegistryError),
    DownloadTarballToStore(#[error(source)] TarballError),
    CreateCasFiles(#[error(source)] CreateCasFilesError),
    SymlinkPackage(#[error(source)] SymlinkPackageError),
}

impl<'a> InstallPackageFromRegistry<'a> {
    /// Execute the subroutine.
    pub async fn run<Tag>(self) -> Result<PackageVersion, InstallPackageFromRegistryError>
    where
        Tag: FromStr + Into<PackageTag>,
    {
        let &InstallPackageFromRegistry { http_client, config, name, version_range, .. } = &self;

        Ok(if let Ok(tag) = version_range.parse::<Tag>() {
            let package_version = PackageVersion::fetch_from_registry(
                name,
                tag.into(),
                http_client,
                &config.registry,
            )
            .await
            .map_err(InstallPackageFromRegistryError::FetchFromRegistry)?;
            self.install_package_version(&package_version).await?;
            package_version
        } else {
            let package = Package::fetch_from_registry(name, http_client, &config.registry)
                .await
                .map_err(InstallPackageFromRegistryError::FetchFromRegistry)?;
            let package_version = package.pinned_version(version_range).unwrap(); // TODO: propagate error for when no version satisfies range
            self.install_package_version(package_version).await?;
            package_version.clone()
        })
    }

    async fn install_package_version(
        self,
        package_version: &PackageVersion,
    ) -> Result<(), InstallPackageFromRegistryError> {
        let InstallPackageFromRegistry {
            tarball_mem_cache,
            http_client,
            io_thread,
            config,
            node_modules_dir,
            ..
        } = self;

        let store_folder_name = package_version.to_virtual_store_name();

        // TODO: skip when it already exists in store?
        let cas_paths = DownloadTarballToStore {
            http_client,
            io_thread,
            store_dir: &config.store_dir,
            package_integrity: package_version
                .dist
                .integrity
                .as_ref()
                .expect("has integrity field"),
            package_unpacked_size: package_version.dist.unpacked_size,
            package_url: package_version.as_tarball_url(),
        }
        .run_with_mem_cache(tarball_mem_cache)
        .await
        .map_err(InstallPackageFromRegistryError::DownloadTarballToStore)?;

        let save_path = config
            .virtual_store_dir
            .join(store_folder_name)
            .join("node_modules")
            .join(&package_version.name);

        let symlink_path = node_modules_dir.join(&package_version.name);

        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_path, "Import package");

        create_cas_files(config.package_import_method, &save_path, &cas_paths)
            .map_err(InstallPackageFromRegistryError::CreateCasFiles)?;

        symlink_package(&save_path, &symlink_path)
            .map_err(InstallPackageFromRegistryError::SymlinkPackage)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_semver::Version;
    use pacquet_npmrc::Npmrc;
    use pacquet_store_dir::StoreDir;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn create_config(store_dir: &Path, modules_dir: &Path, virtual_store_dir: &Path) -> Npmrc {
        Npmrc {
            hoist: false,
            hoist_pattern: vec![],
            public_hoist_pattern: vec![],
            shamefully_hoist: false,
            store_dir: StoreDir::new(store_dir),
            modules_dir: modules_dir.to_path_buf(),
            node_linker: Default::default(),
            symlink: false,
            virtual_store_dir: virtual_store_dir.to_path_buf(),
            package_import_method: Default::default(),
            modules_cache_max_age: 0,
            lockfile: false,
            prefer_frozen_lockfile: false,
            lockfile_include_tarball_url: false,
            registry: "https://registry.npmjs.com/".to_string(),
            auto_install_peers: false,
            dedupe_peer_dependents: false,
            strict_peer_dependencies: false,
            resolve_peers_from_workspace_root: false,
        }
    }

    #[tokio::test]
    pub async fn should_find_package_version_from_registry() {
        let store_dir = tempdir().unwrap();
        let modules_dir = tempdir().unwrap();
        let virtual_store_dir = tempdir().unwrap();
        let config: &'static Npmrc =
            create_config(store_dir.path(), modules_dir.path(), virtual_store_dir.path())
                .pipe(Box::new)
                .pipe(Box::leak);
        let http_client = ThrottledClient::new_from_cpu_count();
        let package = InstallPackageFromRegistry {
            tarball_mem_cache: &Default::default(),
            io_thread: &IoThread::spawn(),
            config,
            http_client: &http_client,
            name: "fast-querystring",
            version_range: "1.0.0",
            node_modules_dir: modules_dir.path(),
        }
        .run::<Version>()
        .await
        .unwrap();

        assert_eq!(package.name, "fast-querystring");
        assert_eq!(
            package.version,
            Version { major: 1, minor: 0, patch: 0, build: vec![], pre_release: vec![] }
        );

        let virtual_store_path = virtual_store_dir
            .path()
            .join(package.to_virtual_store_name())
            .join("node_modules")
            .join(&package.name);
        assert!(virtual_store_path.is_dir());

        // Make sure the symlink is resolving to the correct path
        assert_eq!(
            fs::read_link(modules_dir.path().join(&package.name)).unwrap(),
            virtual_store_path
        );
    }
}
