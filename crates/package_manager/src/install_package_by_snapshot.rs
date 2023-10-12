use crate::{CreateVirtualDirBySnapshot, CreateVirtualDirError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, LockfileResolution, PackageSnapshot, PkgNameVerPeer};
use pacquet_npmrc::Npmrc;
use pacquet_tarball::{download_tarball_to_store, Cache, TarballError};
use pipe_trait::Pipe;
use reqwest::Client;
use std::borrow::Cow;

/// Error type of [`InstallPackageBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageBySnapshotError {
    DownloadTarball(TarballError),
    CreateVirtualDir(CreateVirtualDirError),
}

/// This subroutine downloads a package tarball, extracts it, installs it to a virtual dir,
/// then creates the symlink layout for the package.
#[must_use]
pub struct InstallPackageBySnapshot<'a> {
    pub tarball_cache: &'a Cache,
    pub http_client: &'a Client,
    pub config: &'static Npmrc,
    pub dependency_path: &'a DependencyPath,
    pub package_snapshot: &'a PackageSnapshot,
}

impl<'a> InstallPackageBySnapshot<'a> {
    /// Execute the subroutine.
    pub async fn install(self) -> Result<(), InstallPackageBySnapshotError> {
        let InstallPackageBySnapshot {
            tarball_cache,
            http_client,
            config,
            dependency_path,
            package_snapshot,
        } = self;
        let PackageSnapshot { resolution, .. } = package_snapshot;
        let DependencyPath { custom_registry, package_specifier } = dependency_path;

        let (tarball_url, integrity) = match resolution {
            LockfileResolution::Tarball(tarball_resolution) => {
                let integrity = tarball_resolution.integrity.as_deref().unwrap_or_else(|| {
                    // TODO: how to handle the absent of integrity field?
                    panic!("Current implementation requires integrity, but {dependency_path} doesn't have it");
                });
                (tarball_resolution.tarball.as_str().pipe(Cow::Borrowed), integrity)
            }
            LockfileResolution::Registry(registry_resolution) => {
                let registry = custom_registry.as_ref().unwrap_or(&config.registry);
                let registry = registry.strip_suffix('/').unwrap_or(registry);
                let PkgNameVerPeer { name, suffix: ver_peer } = package_specifier;
                let version = ver_peer.version();
                let bare_name = name.bare.as_str();
                let tarball_url = format!("{registry}/{name}/-/{bare_name}-{version}.tgz");
                let integrity = registry_resolution.integrity.as_str();
                (Cow::Owned(tarball_url), integrity)
            }
            LockfileResolution::Directory(_) | LockfileResolution::Git(_) => {
                panic!("Only TarballResolution and RegistryResolution is supported at the moment, but {dependency_path} requires {resolution:?}");
            }
        };

        // TODO: skip when already exists in store?
        let cas_paths = download_tarball_to_store(
            tarball_cache,
            http_client,
            &config.store_dir,
            integrity,
            None,
            &tarball_url,
        )
        .await
        .map_err(InstallPackageBySnapshotError::DownloadTarball)?;

        CreateVirtualDirBySnapshot {
            dependency_path,
            virtual_store_dir: &config.virtual_store_dir,
            cas_paths: &cas_paths,
            import_method: config.package_import_method,
            package_snapshot,
        }
        .create_virtual_dir_by_snapshot()
        .map_err(InstallPackageBySnapshotError::CreateVirtualDir)?;

        Ok(())
    }
}
