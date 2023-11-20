use crate::{CreateVirtualDirBySnapshot, CreateVirtualDirError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, LockfileResolution, PackageSnapshot, PkgNameVerPeer};
use pacquet_npmrc::Npmrc;
use pacquet_tarball::{DownloadTarballToStore, TarballError};
use pipe_trait::Pipe;
use reqwest::Client;
use std::{borrow::Cow, ffi::OsString, io::ErrorKind};

/// This subroutine downloads a package tarball, extracts it, installs it to a virtual dir,
/// then creates the symlink layout for the package.
#[must_use]
pub struct InstallPackageBySnapshot<'a> {
    pub http_client: &'a Client,
    pub config: &'static Npmrc,
    pub dependency_path: &'a DependencyPath,
    pub package_snapshot: &'a PackageSnapshot,
}

/// Error type of [`InstallPackageBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageBySnapshotError {
    DownloadTarball(TarballError),
    CreateVirtualDir(CreateVirtualDirError),
}

impl<'a> InstallPackageBySnapshot<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), InstallPackageBySnapshotError> {
        let InstallPackageBySnapshot { http_client, config, dependency_path, package_snapshot } =
            self;
        let PackageSnapshot { resolution, .. } = package_snapshot;
        let DependencyPath { custom_registry, package_specifier } = dependency_path;

        let (tarball_url, integrity) = match resolution {
            LockfileResolution::Tarball(tarball_resolution) => {
                let integrity = tarball_resolution.integrity.as_ref().unwrap_or_else(|| {
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
                let integrity = &registry_resolution.integrity;
                (Cow::Owned(tarball_url), integrity)
            }
            LockfileResolution::Directory(_) | LockfileResolution::Git(_) => {
                panic!("Only TarballResolution and RegistryResolution is supported at the moment, but {dependency_path} requires {resolution:?}");
            }
        };

        let store_dir = &config.store_dir;
        let cas_paths = match store_dir.read_index_file(integrity) {
            Ok(index) => store_dir
                .cas_file_paths_by_index(&index)
                .map(|(entry_path, store_path)| (OsString::from(entry_path), store_path))
                .collect(),
            Err(error) => {
                if error.io_error_kind() != Some(ErrorKind::NotFound) {
                    let path = error.file_path();
                    tracing::warn!(
                        target: "pacquet::read_store",
                        ?error,
                        ?path,
                        "Failed to read index from store",
                    );
                }
                DownloadTarballToStore {
                    http_client,
                    store_dir,
                    package_integrity: integrity,
                    package_unpacked_size: None,
                    package_url: &tarball_url,
                }
                .run_without_mem_cache()
                .await
                .map_err(InstallPackageBySnapshotError::DownloadTarball)?
            }
        };

        CreateVirtualDirBySnapshot {
            virtual_store_dir: &config.virtual_store_dir,
            cas_paths: &cas_paths,
            import_method: config.package_import_method,
            dependency_path,
            package_snapshot,
        }
        .run()
        .map_err(InstallPackageBySnapshotError::CreateVirtualDir)?;

        Ok(())
    }
}
