use crate::{CreateVirtualDirBySnapshot, CreateVirtualDirError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{LockfileResolution, PackageKey, PackageMetadata, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::{SharedReadonlyStoreIndex, StoreIndexWriter};
use pacquet_tarball::{DownloadTarballToStore, TarballError};
use pipe_trait::Pipe;
use std::{borrow::Cow, sync::Arc};

/// This subroutine downloads a package tarball, extracts it, installs it to a virtual dir,
/// then creates the symlink layout for the package.
#[must_use]
pub struct InstallPackageBySnapshot<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub store_index: Option<&'a SharedReadonlyStoreIndex>,
    pub store_index_writer: Option<&'a Arc<StoreIndexWriter>>,
    pub package_key: &'a PackageKey,
    pub metadata: &'a PackageMetadata,
    pub snapshot: &'a SnapshotEntry,
}

/// Error type of [`InstallPackageBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageBySnapshotError {
    #[diagnostic(transparent)]
    DownloadTarball(#[error(source)] TarballError),

    #[diagnostic(transparent)]
    CreateVirtualDir(#[error(source)] CreateVirtualDirError),

    #[display(
        "Package `{package_key}` has a tarball resolution without an `integrity` field; pacquet cannot verify the download and refuses to install it."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_tarball_integrity))]
    MissingTarballIntegrity { package_key: String },

    #[display(
        "Package `{package_key}` uses a `{resolution_kind}` resolution, which pacquet does not yet support."
    )]
    #[diagnostic(code(pacquet_package_manager::unsupported_resolution))]
    UnsupportedResolution { package_key: String, resolution_kind: &'static str },
}

impl<'a> InstallPackageBySnapshot<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), InstallPackageBySnapshotError> {
        let InstallPackageBySnapshot {
            http_client,
            config,
            store_index,
            store_index_writer,
            package_key,
            metadata,
            snapshot,
        } = self;

        let (tarball_url, integrity) = match &metadata.resolution {
            LockfileResolution::Tarball(tarball_resolution) => {
                let integrity = tarball_resolution.integrity.as_ref().ok_or_else(|| {
                    InstallPackageBySnapshotError::MissingTarballIntegrity {
                        package_key: package_key.to_string(),
                    }
                })?;
                (tarball_resolution.tarball.as_str().pipe(Cow::Borrowed), integrity)
            }
            LockfileResolution::Registry(registry_resolution) => {
                let registry = config.registry.strip_suffix('/').unwrap_or(&config.registry);
                let name = &package_key.name;
                let version = package_key.suffix.version();
                let bare_name = name.bare.as_str();
                let tarball_url = format!("{registry}/{name}/-/{bare_name}-{version}.tgz");
                let integrity = &registry_resolution.integrity;
                (Cow::Owned(tarball_url), integrity)
            }
            LockfileResolution::Directory(_) => {
                return Err(InstallPackageBySnapshotError::UnsupportedResolution {
                    package_key: package_key.to_string(),
                    resolution_kind: "directory",
                });
            }
            LockfileResolution::Git(_) => {
                return Err(InstallPackageBySnapshotError::UnsupportedResolution {
                    package_key: package_key.to_string(),
                    resolution_kind: "git",
                });
            }
        };

        // TODO: skip when already exists in store?
        let package_id = package_key.without_peer().to_string();
        let cas_paths = DownloadTarballToStore {
            http_client,
            store_dir: &config.store_dir,
            store_index: store_index.cloned(),
            store_index_writer: store_index_writer.cloned(),
            verify_store_integrity: config.verify_store_integrity,
            package_integrity: integrity,
            package_unpacked_size: None,
            package_url: &tarball_url,
            package_id: &package_id,
        }
        .run_without_mem_cache()
        .await
        .map_err(InstallPackageBySnapshotError::DownloadTarball)?;

        CreateVirtualDirBySnapshot {
            virtual_store_dir: &config.virtual_store_dir,
            cas_paths: &cas_paths,
            import_method: config.package_import_method,
            package_key,
            snapshot,
        }
        .run()
        .map_err(InstallPackageBySnapshotError::CreateVirtualDir)?;

        Ok(())
    }
}
