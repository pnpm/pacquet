use crate::{
    create_symlink_layout, store_init::init_store_dir_best_effort, InstallPackageBySnapshot,
    InstallPackageBySnapshotError, SymlinkPackageError,
};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_lockfile::{PackageKey, PackageMetadata, PkgName, SnapshotDepRef, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::{StoreIndex, StoreIndexWriter};
use rayon::prelude::*;
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
}

/// Error type of [`CreateVirtualStore`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualStoreError {
    #[diagnostic(transparent)]
    InstallPackageBySnapshot(#[error(source)] InstallPackageBySnapshotError),

    #[display("Failed to recursively create node_modules directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::create_node_modules_dir))]
    CreateNodeModulesDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[diagnostic(transparent)]
    SymlinkPackage(#[error(source)] SymlinkPackageError),

    #[display("Symlink task panicked or was cancelled: {error}")]
    #[diagnostic(code(pacquet_package_manager::symlink_task_join))]
    SymlinkTaskJoin {
        #[error(source)]
        error: tokio::task::JoinError,
    },

    #[display("Lockfile has a snapshot entry `{snapshot_key}` with no matching metadata entry (`{metadata_key}`) in `packages:`.")]
    #[diagnostic(code(pacquet_package_manager::missing_package_metadata))]
    MissingPackageMetadata { snapshot_key: String, metadata_key: String },

    #[display(
        "Lockfile has a `snapshots:` section but no `packages:` section; every entry in `snapshots:` must have a matching metadata entry. The lockfile is malformed."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_packages_section))]
    MissingPackagesSection,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), CreateVirtualStoreError> {
        let CreateVirtualStore { http_client, config, packages, snapshots } = self;

        let Some(snapshots) = snapshots else {
            // No snapshots to install. If the lockfile also has no project deps
            // this is a valid no-op; if it does, pnpm would have populated
            // `snapshots`, so bailing out here is safe enough for v9.
            return Ok(());
        };
        let packages = packages.ok_or(CreateVirtualStoreError::MissingPackagesSection)?;

        // Open the read-only SQLite index once for the whole run instead of
        // per snapshot. Every `InstallPackageBySnapshot` performs a cache
        // lookup against this index before falling through to the network;
        // on a 1352-package lockfile the per-snapshot reopen accounted for
        // ~1.3 s of wall time even with a fully populated store (see #260).
        // A `None` here means the store has no `index.db` yet (first install
        // against an empty store), in which case every lookup would miss —
        // so we keep the handle `Option`al and short-circuit.
        //
        // The open itself is synchronous SQLite I/O (`Connection::open_with_flags`
        // + a `PRAGMA busy_timeout`), so park it on the blocking pool instead
        // of stalling the reactor thread, even for the sub-millisecond it
        // usually takes.
        //
        // A `JoinError` here (blocking-task panic, or cancellation during
        // runtime shutdown) is degraded into `None` so the install still
        // makes progress — cache lookups just miss. `shared_readonly_in`
        // already yields `None` for a first-time install against an empty
        // store, and downstream callers handle that shape correctly. We
        // surface the error at `warn!` so a silent task panic or
        // cancellation is still diagnosable in the log.
        let store_dir: &'static _ = &config.store_dir;

        // Eagerly create `files/00..ff` under the v11 store root so per-
        // tarball CAFS writes never pay a `create_dir_all` syscall on the
        // hot path. Ports pnpm's `initStore` in `worker/src/start.ts`.
        // See [`init_store_dir_best_effort`] for the error-degradation
        // policy shared with `install_without_lockfile.rs`.
        init_store_dir_best_effort(store_dir).await;

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

        // Spawn the batched store-index writer. A single `spawn_blocking`
        // task owns the writable SQLite connection for the whole install;
        // every successfully extracted tarball just sends a row to it and
        // the task flushes them in batched transactions. The old per-
        // tarball `StoreIndex::open` + solo-INSERT pattern dominated
        // install wall time on slow-metadata filesystems (#263) because
        // each open is ~15 ms of metadata work on APFS and tokio's
        // blocking pool grew to 500+ threads to service them.
        //
        // We drop our own copy of the `Arc<StoreIndexWriter>` after the
        // `try_join!` below so the channel can close once every tarball
        // task has dropped its clone; then `.await` on the join handle
        // waits for the final batch to flush before returning. A writer-
        // side `JoinError` or open failure is surfaced at `warn!` and
        // degraded to "no writer" — the install still succeeds, missing
        // rows just force a re-download on the next install.
        let (store_index_writer, writer_task) = StoreIndexWriter::spawn(store_dir);
        let store_index_writer_ref = Some(&store_index_writer);

        // `config: &'static Npmrc`, so `virtual_store_dir` is borrowable for
        // the full process lifetime — we lean on that to move the whole
        // symlink pass into a `spawn_blocking` task below without having to
        // clone the root path per snapshot.
        let virtual_store_dir: &'static Path = &config.virtual_store_dir;

        // Collect owned per-snapshot data for the symlink branch. Only the
        // dep map (`Option<HashMap<PkgName, SnapshotDepRef>>`) is cloned;
        // the rest is small owned values. For the 1352-package fixture
        // this adds up to a few tens of ms of one-off clone work and moves
        // the whole symlink pass onto the blocking pool with no 'static /
        // runtime-flavor fragility. `SnapshotDepRef::resolve` and path
        // composition stay inside `create_symlink_layout` so the per-dep
        // logic is trivial to cross-reference against pnpm.
        let snapshot_symlink_tasks: Vec<SnapshotSymlinkTask> = snapshots
            .iter()
            .map(|(key, snapshot)| SnapshotSymlinkTask {
                virtual_node_modules_dir: virtual_node_modules_dir_for(virtual_store_dir, key),
                dependencies: snapshot.dependencies.clone(),
            })
            .collect();

        // Run intra-package symlink creation concurrently with tarball fetch
        // + CAS import. Symlinks only depend on the resolved dep graph, not
        // on any tarball contents, so there's no reason to serialise them
        // behind downloads.
        //
        // Matches pnpm's `headless` installer, which forks the same two
        // branches off the resolved graph:
        //
        //   await Promise.all([linkAllModules(...), linkAllPkgs(...)])
        //
        // See `pnpm/pkg-manager/headless/src/index.ts` and
        // `pnpm/pkg-manager/core/src/install/link.ts`. We dispatch the whole
        // symlink branch through a single `spawn_blocking` so a rayon
        // `par_iter` over all snapshots does the real work on rayon's pool;
        // the tokio task just awaits the join handle, which cannot starve
        // `fetch_fut` and is safe on any runtime flavor.
        let symlink_handle =
            tokio::task::spawn_blocking(move || -> Result<(), CreateVirtualStoreError> {
                snapshot_symlink_tasks.par_iter().try_for_each(|task| {
                    std::fs::create_dir_all(&task.virtual_node_modules_dir).map_err(|error| {
                        CreateVirtualStoreError::CreateNodeModulesDir {
                            dir: task.virtual_node_modules_dir.clone(),
                            error,
                        }
                    })?;
                    if let Some(dependencies) = &task.dependencies {
                        create_symlink_layout(
                            dependencies,
                            virtual_store_dir,
                            &task.virtual_node_modules_dir,
                        )
                        .map_err(CreateVirtualStoreError::SymlinkPackage)?;
                    }
                    Ok(())
                })
            });
        let symlink_fut = async {
            match symlink_handle.await {
                Ok(result) => result,
                Err(error) => Err(CreateVirtualStoreError::SymlinkTaskJoin { error }),
            }
        };

        let fetch_fut = async {
            future::try_join_all(snapshots.keys().map(|snapshot_key| async move {
                let metadata_key = snapshot_key.without_peer();
                let metadata = packages.get(&metadata_key).ok_or_else(|| {
                    CreateVirtualStoreError::MissingPackageMetadata {
                        snapshot_key: snapshot_key.to_string(),
                        metadata_key: metadata_key.to_string(),
                    }
                })?;
                InstallPackageBySnapshot {
                    http_client,
                    config,
                    store_index: store_index_ref,
                    store_index_writer: store_index_writer_ref,
                    package_key: snapshot_key,
                    metadata,
                }
                .run()
                .await
                .map_err(CreateVirtualStoreError::InstallPackageBySnapshot)
            }))
            .await
            .map(drop)
        };

        tokio::try_join!(symlink_fut, fetch_fut)?;

        // Drop the orchestration's sender so the channel closes once every
        // per-tarball clone has also dropped; then wait for the writer task
        // to flush its final batch. Swallow any error with `warn!` — we've
        // already done the install and cache-miss degradation is fine.
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

/// Owned snapshot data handed off to the symlink `spawn_blocking` task.
///
/// Kept deliberately small: just the virtual-store package dir path and a
/// cloned copy of the snapshot's `dependencies` map. The rest of the data
/// `create_symlink_layout` needs (virtual store root, `PkgName`/`SnapshotDepRef`
/// resolution) is either `'static`-borrowable or lives inside this clone.
struct SnapshotSymlinkTask {
    virtual_node_modules_dir: PathBuf,
    dependencies: Option<HashMap<PkgName, SnapshotDepRef>>,
}

fn virtual_node_modules_dir_for(virtual_store_dir: &Path, package_key: &PackageKey) -> PathBuf {
    virtual_store_dir.join(package_key.to_virtual_store_name()).join("node_modules")
}
