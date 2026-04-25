use crate::{
    store_init::init_store_dir_best_effort, InstallPackageBySnapshot, InstallPackageBySnapshotError,
};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_lockfile::{LockfileResolution, PackageKey, PackageMetadata, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_store_dir::{store_index_key, StoreIndex, StoreIndexWriter};
use pacquet_tarball::prefetch_cas_paths;
use pipe_trait::Pipe;
use std::collections::HashMap;

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
        // `try_join_all` below so the channel can close once every tarball
        // task has dropped its clone; then `.await` on the join handle
        // waits for the final batch to flush before returning. A writer-
        // side `JoinError` or open failure is surfaced at `warn!` and
        // degraded to "no writer" — the install still succeeds, missing
        // rows just force a re-download on the next install.
        let (store_index_writer, writer_task) = StoreIndexWriter::spawn(store_dir);
        let store_index_writer_ref = Some(&store_index_writer);

        // Batch every cache lookup the per-snapshot futures would otherwise
        // each fan into `tokio::task::spawn_blocking`. With 1352 snapshots
        // hitting the default 512-thread blocking pool, each task's actual
        // work (≈40 µs SELECT + per-file integrity stats) gets dwarfed by
        // OS context-switching among hundreds of competing threads
        // (sample-profiling: 20-60 ms wall per call, sum 26-82 s). Doing
        // the same `SELECT`s and integrity checks on one thread holding the
        // index mutex once is dramatically faster — and turns each
        // per-snapshot future's cache lookup into a synchronous
        // `HashMap::get`.
        //
        // Compute the cache keys upfront from `(integrity, pkg_id)` for
        // every snapshot whose metadata has a tarball-style resolution.
        // Tarball-and-Registry resolutions both ship an `Integrity`;
        // Directory and Git resolutions don't go through CAFS at all,
        // so skipping them here matches the per-snapshot path's check.
        //
        // Lockfiles with peer-dependency variants of the same package
        // (e.g. `react-dom@17.0.2(react@17.0.2)` plus
        // `react-dom@17.0.2(react@18.2.0)`) collapse to one cache key
        // here because the key is built from `metadata_key.without_peer()`.
        // Sort + dedup so `prefetch_cas_paths` doesn't redo identical
        // SELECT + integrity-check work for every peer variant
        // (Copilot review on #292).
        let mut cache_keys: Vec<String> = Vec::with_capacity(snapshots.len());
        for snapshot_key in snapshots.keys() {
            let metadata_key = snapshot_key.without_peer();
            let Some(metadata) = packages.get(&metadata_key) else { continue };
            let integrity_string = match &metadata.resolution {
                LockfileResolution::Tarball(t) => t.integrity.as_ref().map(|i| i.to_string()),
                LockfileResolution::Registry(r) => Some(r.integrity.to_string()),
                LockfileResolution::Directory(_) | LockfileResolution::Git(_) => continue,
            };
            let Some(integrity) = integrity_string else { continue };
            let pkg_id = metadata_key.to_string();
            cache_keys.push(store_index_key(&integrity, &pkg_id));
        }
        cache_keys.sort_unstable();
        cache_keys.dedup();
        let prefetched = prefetch_cas_paths(
            store_index.clone(),
            store_dir,
            cache_keys,
            config.verify_store_integrity,
        )
        .await;

        // Partition snapshots by whether the prefetch covered them. The
        // warm batch — every snapshot whose tarball is already in the
        // CAFS — runs entirely on rayon: no tokio futures, no
        // `try_join_all` polling overhead, no `spawn_blocking` round-trip
        // per snapshot. The cold batch (cache miss → download needed)
        // keeps the existing `try_join_all` + download path.
        //
        // **Why this beats per-snapshot tokio futures:** profiling at
        // 1352 prefetched / 0 cold on a 10-core Mac showed `sum-of-link
        // ≈ wall` (~10 s sum on a 10 s wall, i.e. effectively 1×
        // parallelism) even though `try_join_all` was meant to fan
        // futures across tokio's 10 worker threads. Each future's sync
        // `rayon::join` pinned one tokio worker; with up to 10 such
        // futures progressing concurrently, each one's inner par_iter
        // saturated rayon's pool, and the pool ended up processing one
        // snapshot at a time. Going straight to rayon via a single
        // `par_iter` lets the pool schedule across all 1352 snapshots
        // as one work-stealing graph — the shape pnpm's piscina pool
        // gives implicitly. On the same benchmark, wall dropped from
        // ~10 s to ~6.5 s.
        //
        // The `par_iter` blocks the calling thread for the duration of
        // the warm batch. The cold-batch fetches run *after* this
        // returns; that ordering is intentional — warm-cache work has
        // no network dependency, so we'd be racing a cold download
        // against a CPU/syscall-bound rayon batch for nothing.
        type CasPathsArc = std::sync::Arc<HashMap<String, std::path::PathBuf>>;
        type WarmEntry<'a> = (&'a PackageKey, &'a SnapshotEntry, &'a CasPathsArc);
        type ColdEntry<'a> = (&'a PackageKey, &'a SnapshotEntry);
        let mut warm: Vec<WarmEntry<'_>> = Vec::with_capacity(snapshots.len());
        let mut cold: Vec<ColdEntry<'_>> = Vec::new();
        for (snapshot_key, snapshot) in snapshots.iter() {
            let metadata_key = snapshot_key.without_peer();
            let Some(metadata) = packages.get(&metadata_key) else {
                cold.push((snapshot_key, snapshot));
                continue;
            };
            let integrity_string = match &metadata.resolution {
                LockfileResolution::Tarball(t) => t.integrity.as_ref().map(|i| i.to_string()),
                LockfileResolution::Registry(r) => Some(r.integrity.to_string()),
                LockfileResolution::Directory(_) | LockfileResolution::Git(_) => None,
            };
            let Some(integrity) = integrity_string else {
                cold.push((snapshot_key, snapshot));
                continue;
            };
            let pkg_id = metadata_key.to_string();
            let cache_key = store_index_key(&integrity, &pkg_id);
            if let Some(cas_paths) = prefetched.get(&cache_key) {
                warm.push((snapshot_key, snapshot, cas_paths));
            } else {
                cold.push((snapshot_key, snapshot));
            }
        }

        let virtual_store_dir = &config.virtual_store_dir;
        let import_method = config.package_import_method;
        {
            use rayon::prelude::*;
            warm.par_iter().try_for_each(|(snapshot_key, snapshot, cas_paths)| {
                crate::CreateVirtualDirBySnapshot {
                    virtual_store_dir,
                    cas_paths: cas_paths.as_ref(),
                    import_method,
                    package_key: snapshot_key,
                    snapshot,
                }
                .run()
                .map_err(|e| {
                    CreateVirtualStoreError::InstallPackageBySnapshot(
                        InstallPackageBySnapshotError::CreateVirtualDir(e),
                    )
                })
            })?;
        }

        // Cold batch: snapshots that didn't prefetch — fall through to the
        // existing tokio + download path.
        if !cold.is_empty() {
            let prefetched_ref = Some(&prefetched);
            cold.iter()
                .map(|(snapshot_key, snapshot)| async move {
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
                        prefetched_cas_paths: prefetched_ref,
                        package_key: snapshot_key,
                        metadata,
                        snapshot,
                    }
                    .run()
                    .await
                    .map_err(CreateVirtualStoreError::InstallPackageBySnapshot)
                })
                .pipe(future::try_join_all)
                .await?;
        }

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
