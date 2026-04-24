use crate::{InstallPackageFromRegistry, InstallPackageFromRegistryError};
use async_recursion::async_recursion;
use dashmap::DashSet;
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use node_semver::Version;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::PackageVersion;
use pacquet_store_dir::StoreIndex;
use pacquet_tarball::MemCache;
use pipe_trait::Pipe;

/// In-memory cache for packages that have started resolving dependencies.
///
/// The contents of set is the package's virtual_store_name.
/// e.g. `@pnpm.e2e/dep-1@1.0.0` →  `@pnpm.e2e+dep-1@1.0.0`
pub type ResolvedPackages = DashSet<String>;

/// This subroutine install packages from a `package.json` without reading or writing a lockfile.
///
/// **Brief overview for each package:**
/// * Fetch a tarball of the package.
/// * Extract the tarball into the store directory.
/// * Import (by reflink, hardlink, or copy) the files from the store dir to `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at `node_modules/{name}`.
/// * Repeat the process for the dependencies of the package.
#[must_use]
pub struct InstallWithoutLockfile<'a, DependencyGroupList> {
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub dependency_groups: DependencyGroupList,
}

/// Error type of [`InstallWithoutLockfile`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallWithoutLockfileError {
    #[diagnostic(transparent)]
    InstallPackageFromRegistry(#[error(source)] InstallPackageFromRegistryError),
}

impl<'a, DependencyGroupList> InstallWithoutLockfile<'a, DependencyGroupList> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), InstallWithoutLockfileError>
    where
        DependencyGroupList: IntoIterator<Item = DependencyGroup>,
    {
        let InstallWithoutLockfile {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            dependency_groups,
            resolved_packages,
        } = self;

        // Open the read-only SQLite index once per install, shared across
        // every `DownloadTarballToStore`. See the matching comment in
        // `create_virtual_store.rs` for the why.
        let store_index = StoreIndex::shared_readonly_in(&config.store_dir);
        let store_index_ref = store_index.as_ref();

        manifest
            .dependencies(dependency_groups)
            .map(|(name, version_range)| async move {
                let dependency = InstallPackageFromRegistry {
                    tarball_mem_cache,
                    http_client,
                    config,
                    store_index: store_index_ref,
                    node_modules_dir: &config.modules_dir,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .map_err(InstallWithoutLockfileError::InstallPackageFromRegistry)?;

                InstallWithoutLockfile {
                    tarball_mem_cache,
                    http_client,
                    config,
                    manifest,
                    dependency_groups: (),
                    resolved_packages,
                }
                .install_dependencies_from_registry(&dependency, store_index_ref)
                .await?;

                Ok::<_, InstallWithoutLockfileError>(())
            })
            .pipe(future::try_join_all)
            .await?;

        Ok(())
    }
}

impl<'a> InstallWithoutLockfile<'a, ()> {
    /// Install dependencies of a dependency.
    #[async_recursion]
    async fn install_dependencies_from_registry(
        &self,
        package: &PackageVersion,
        store_index: Option<&'async_recursion pacquet_store_dir::SharedReadonlyStoreIndex>,
    ) -> Result<(), InstallWithoutLockfileError> {
        let InstallWithoutLockfile {
            tarball_mem_cache,
            http_client,
            config,
            resolved_packages,
            ..
        } = self;

        // This package has already resolved, there is no need to reinstall again.
        if !resolved_packages.insert(package.to_virtual_store_name()) {
            tracing::info!(target: "pacquet::install", package = ?package.to_virtual_store_name(), "Skip subset");
            return Ok(());
        }

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
                    tarball_mem_cache,
                    http_client,
                    config,
                    store_index,
                    node_modules_dir: &node_modules_path,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .map_err(InstallWithoutLockfileError::InstallPackageFromRegistry)?;
                self.install_dependencies_from_registry(&dependency, store_index).await?;
                Ok::<_, InstallWithoutLockfileError>(())
            })
            .pipe(future::try_join_all)
            .await?;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");

        Ok(())
    }
}
