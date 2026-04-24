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
use pacquet_store_dir::{StoreIndex, StoreIndexWriter};
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

        let store_dir: &'static _ = &config.store_dir;

        // Eagerly create `files/00..ff` under the v11 store root so per-
        // tarball CAFS writes never pay a `create_dir_all` syscall on the
        // hot path. Ports pnpm's `initStore` in
        // `worker/src/start.ts`, gated by the same `files/` existence
        // check (`StoreDir::init` handles the gating internally). Any
        // failure here is degraded to a `warn!` — the lazy per-shard
        // fallback inside `StoreDir::write_cas_file` will still mkdir
        // on demand, matching pnpm's `writeFile.ts` `dirs` Set.
        //
        // Two error layers to handle separately: an outer `JoinError`
        // means the blocking task panicked or was cancelled; an inner
        // `io::Error` means `StoreDir::init` itself failed (permission
        // denied, disk full, …). Both get the same warning so they
        // stay diagnosable.
        match tokio::task::spawn_blocking(move || store_dir.init()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    target: "pacquet::install",
                    ?error,
                    "store-dir init failed; continuing — write-side lazy mkdir fallback will handle it",
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "pacquet::install",
                    ?error,
                    "store-dir init task panicked or was cancelled; continuing — write-side lazy mkdir fallback will handle it",
                );
            }
        }

        // Open the read-only SQLite index once per install, shared across
        // every `DownloadTarballToStore`. See the matching comment in
        // `create_virtual_store.rs` for the full rationale, including the
        // `JoinError`-to-cache-miss degradation (with a `warn!` so it
        // stays diagnosable).
        let store_index =
            match tokio::task::spawn_blocking(move || StoreIndex::shared_readonly_in(store_dir))
                .await
            {
                Ok(store_index) => store_index,
                Err(error) => {
                    tracing::warn!(
                        target: "pacquet::install",
                        ?error,
                        "store-index open task failed; continuing without a shared cache index",
                    );
                    None
                }
            };
        let store_index_ref = store_index.as_ref();

        // Batched store-index writer. See `create_virtual_store.rs` for
        // the full rationale — we spawn once, every tarball just queues a
        // row, and one writer task flushes them in batched transactions.
        let (store_index_writer, writer_task) = StoreIndexWriter::spawn(store_dir);
        let store_index_writer_ref = Some(&store_index_writer);

        manifest
            .dependencies(dependency_groups)
            .map(|(name, version_range)| async move {
                let dependency = InstallPackageFromRegistry {
                    tarball_mem_cache,
                    http_client,
                    config,
                    store_index: store_index_ref,
                    store_index_writer: store_index_writer_ref,
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
                .install_dependencies_from_registry(
                    &dependency,
                    store_index_ref,
                    store_index_writer_ref,
                )
                .await?;

                Ok::<_, InstallWithoutLockfileError>(())
            })
            .pipe(future::try_join_all)
            .await?;

        // Drop the orchestration's writer handle so the channel closes,
        // then wait for the final batch flush. See `create_virtual_store.rs`
        // for why errors here are downgraded to `warn!`.
        drop(store_index_writer);
        match writer_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(
                target: "pacquet::install",
                ?error,
                "store-index writer task returned an error; some rows may not be persisted",
            ),
            Err(error) => tracing::warn!(
                target: "pacquet::install",
                ?error,
                "store-index writer task panicked; some rows may not be persisted",
            ),
        }

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
        store_index_writer: Option<
            &'async_recursion std::sync::Arc<pacquet_store_dir::StoreIndexWriter>,
        >,
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

        let node_modules_path_ref = &node_modules_path;
        package
            .dependencies(self.config.auto_install_peers)
            .map(|(name, version_range)| async move {
                let dependency = InstallPackageFromRegistry {
                    tarball_mem_cache,
                    http_client,
                    config,
                    store_index,
                    store_index_writer,
                    node_modules_dir: node_modules_path_ref,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .map_err(InstallWithoutLockfileError::InstallPackageFromRegistry)?;
                self.install_dependencies_from_registry(
                    &dependency,
                    store_index,
                    store_index_writer,
                )
                .await?;
                Ok::<_, InstallWithoutLockfileError>(())
            })
            .pipe(future::try_join_all)
            .await?;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");

        Ok(())
    }
}
