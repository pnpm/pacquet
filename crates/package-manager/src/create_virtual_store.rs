use crate::{
    InstallPackageBySnapshot, InstallPackageBySnapshotError, store_init::init_store_dir_best_effort,
};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_lockfile::{LockfileResolution, PackageKey, PackageMetadata, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_reporter::{LogEvent, LogLevel, ProgressLog, ProgressMessage, Reporter};
use pacquet_store_dir::{SharedVerifiedFilesCache, StoreIndex, StoreIndexWriter, store_index_key};
use pacquet_tarball::prefetch_cas_paths;
use pipe_trait::Pipe;
use std::{collections::HashMap, sync::atomic::AtomicU8};

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    /// Install-scoped dedupe state for `pnpm:package-import-method`.
    /// See `link_file::log_method_once`.
    pub logged_methods: &'a AtomicU8,
    /// Install root, threaded into reporter `requester` fields.
    pub requester: &'a str,
}

/// Error type of [`CreateVirtualStore`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualStoreError {
    #[diagnostic(transparent)]
    InstallPackageBySnapshot(#[error(source)] InstallPackageBySnapshotError),

    #[display(
        "Lockfile has a snapshot entry `{snapshot_key}` with no matching metadata entry (`{metadata_key}`) in `packages:`."
    )]
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
    pub async fn run<R: Reporter>(self) -> Result<(), CreateVirtualStoreError> {
        let CreateVirtualStore {
            http_client,
            config,
            packages,
            snapshots,
            logged_methods,
            requester,
        } = self;

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

        // Install-scoped `verifiedFilesCache`. One `Arc<DashSet>` lives
        // for the duration of the install; every per-snapshot fetch
        // gets the same handle. A CAFS path verified on snapshot A
        // populates the set so snapshot B's verify pass skips the stat
        // / re-hash cost. Ports pnpm's `verifiedFilesCache: Set<string>`
        // threading in `store/cafs/src/checkPkgFilesIntegrity.ts`.
        let verified_files_cache = SharedVerifiedFilesCache::default();

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
        // [`snapshot_cache_key`] is the shared key-derivation helper —
        // a future change to the resolution-type handling or key
        // shape stays in one place (Copilot review on #292).
        //
        // Walk `snapshots` once, stash the per-snapshot cache key
        // alongside its `(snapshot_key, snapshot)` tuple, and reuse
        // the stashed key for both the prefetch input and the
        // warm/cold partition below. A separate pass to recompute
        // each key would re-allocate two strings per snapshot for
        // nothing (Copilot follow-up review on #292).
        //
        // Lockfiles with peer-dependency variants of the same package
        // (e.g. `react-dom@17.0.2(react@17.0.2)` plus
        // `react-dom@17.0.2(react@18.2.0)`) collapse to one cache key
        // because the key is built from `metadata_key.without_peer()`.
        // Sort + dedup the prefetch input so `prefetch_cas_paths`
        // doesn't redo identical SELECT + integrity-check work for
        // every peer variant.
        // Validate every snapshot upfront so a malformed lockfile
        // (missing metadata, missing tarball integrity, currently-
        // unsupported directory / git resolution) errors out *before*
        // we start the warm batch. Previously we collapsed those
        // cases into `None` and let them fall through to the cold
        // batch, which meant the warm rayon batch ran to completion
        // (~6 s on `alot7`) before the actual error fired.
        type SnapshotWithCacheKey<'a> = (&'a PackageKey, &'a SnapshotEntry, Option<String>);
        let snapshot_entries: Vec<SnapshotWithCacheKey<'_>> = snapshots
            .iter()
            .map(|(snapshot_key, snapshot)| {
                snapshot_cache_key(snapshot_key, packages).map(|key| (snapshot_key, snapshot, key))
            })
            .collect::<Result<_, _>>()?;
        let mut cache_key_refs: Vec<&str> =
            snapshot_entries.iter().filter_map(|(_, _, k)| k.as_deref()).collect();
        cache_key_refs.sort_unstable();
        cache_key_refs.dedup();
        let cache_keys: Vec<String> = cache_key_refs.into_iter().map(String::from).collect();
        let prefetched = prefetch_cas_paths(
            store_index.clone(),
            store_dir,
            cache_keys,
            config.verify_store_integrity,
            SharedVerifiedFilesCache::clone(&verified_files_cache),
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
        // Element types are inferred from the push calls below — no
        // explicit alias, so the warm tuple's third field stays bound
        // to whatever value type `pacquet_tarball::PrefetchedCasPaths`
        // exposes. A future change there propagates here without a
        // local alias drifting (Copilot review on #292).
        let mut warm = Vec::with_capacity(snapshot_entries.len());
        let mut cold: Vec<(&PackageKey, &SnapshotEntry)> = Vec::new();
        for (snapshot_key, snapshot, cache_key) in &snapshot_entries {
            match cache_key.as_deref().and_then(|k| prefetched.get(k)) {
                Some(cas_paths) => warm.push((snapshot_key, snapshot, cas_paths)),
                None => cold.push((snapshot_key, snapshot)),
            }
        }

        let virtual_store_dir = &config.virtual_store_dir;
        let import_method = config.package_import_method;
        {
            use rayon::prelude::*;
            // Driving the warm batch from inside an `async fn` means
            // the `par_iter` blocks the calling tokio worker for the
            // duration. On the production multi-thread runtime that's
            // fine — `block_in_place` tells the runtime to migrate any
            // other futures off this worker first, so async progress
            // continues on the other workers — but `block_in_place`
            // panics on `current_thread` runtimes, which is what
            // `#[tokio::test]` defaults to. Detect the flavor and only
            // call `block_in_place` when it's safe; on
            // `current_thread` we fall back to a plain inline call,
            // matching how the rest of the test suite already runs
            // sync work directly on the test thread (Copilot review on
            // #292).
            let warm_work = move || {
                warm.par_iter().try_for_each(|(snapshot_key, snapshot, cas_paths)| {
                    let package_id = snapshot_key.without_peer().to_string();

                    // Frozen-lockfile snapshots are "already resolved" by
                    // virtue of being in the lockfile; emit `resolved` per
                    // snapshot to mirror pnpm's per-package counters. A
                    // warm snapshot's `pnpm:progress found_in_store` event
                    // would normally come from `DownloadTarballToStore`;
                    // since the prefetch already settled the bytes for
                    // these, emit it here too so the consumer sees the
                    // full resolved → found_in_store → imported sequence
                    // even when the cold path is skipped.
                    R::emit(&LogEvent::Progress(ProgressLog {
                        level: LogLevel::Debug,
                        message: ProgressMessage::Resolved {
                            package_id: package_id.clone(),
                            requester: requester.to_owned(),
                        },
                    }));
                    R::emit(&LogEvent::Progress(ProgressLog {
                        level: LogLevel::Debug,
                        message: ProgressMessage::FoundInStore {
                            package_id: package_id.clone(),
                            requester: requester.to_owned(),
                        },
                    }));

                    crate::CreateVirtualDirBySnapshot {
                        virtual_store_dir,
                        cas_paths: cas_paths.as_ref(),
                        import_method,
                        logged_methods,
                        requester,
                        package_id: &package_id,
                        package_key: snapshot_key,
                        snapshot,
                    }
                    .run::<R>()
                    .map_err(|e| {
                        CreateVirtualStoreError::InstallPackageBySnapshot(
                            InstallPackageBySnapshotError::CreateVirtualDir(e),
                        )
                    })
                })
            };
            let on_multi_thread = tokio::runtime::Handle::try_current()
                .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
            if on_multi_thread {
                tokio::task::block_in_place(warm_work)?;
            } else {
                warm_work()?;
            }
        }

        // Cold batch: snapshots that didn't prefetch — fall through to the
        // existing tokio + download path.
        if !cold.is_empty() {
            let prefetched_ref = Some(&prefetched);
            let verified_files_cache_ref = &verified_files_cache;
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
                        verified_files_cache: verified_files_cache_ref,
                        logged_methods,
                        requester,
                        package_key: snapshot_key,
                        metadata,
                        snapshot,
                    }
                    .run::<R>()
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

/// Build the store-index cache key for a snapshot.
///
/// Returns:
/// - `Ok(Some(key))` for tarball / registry resolutions with a valid
///   integrity, the only shape that participates in the CAFS prefetch
///   today.
/// - `Err(...)` for any condition the install was previously going to
///   fail on anyway — missing metadata, missing tarball integrity, or
///   a directory / git resolution this build doesn't support yet —
///   so the orchestrator can short-circuit *before* the warm rayon
///   batch runs (Copilot review on #292). The previous shape collapsed
///   these into `None` and shoved them into the cold batch, which
///   meant a malformed lockfile would do up to ~6 s of warm-batch
///   linking before the actual error fired.
/// - `Ok(None)` is currently unused but reserved for any future
///   resolution variant that legitimately doesn't go through CAFS
///   (e.g. workspace `link:`-style deps when those land); without
///   it, adding such a variant later would force a wider refactor.
///
/// Shared by the upfront prefetch-keys loop and the warm/cold
/// partition in [`CreateVirtualStore::run`], so a future change to
/// the resolution-type handling or key shape stays in one place.
/// A drift between the two loops would silently misclassify warm
/// entries as cold and quietly halve install speed.
fn snapshot_cache_key(
    snapshot_key: &PackageKey,
    packages: &HashMap<PackageKey, PackageMetadata>,
) -> Result<Option<String>, CreateVirtualStoreError> {
    let metadata_key = snapshot_key.without_peer();
    let metadata = packages.get(&metadata_key).ok_or_else(|| {
        CreateVirtualStoreError::MissingPackageMetadata {
            snapshot_key: snapshot_key.to_string(),
            metadata_key: metadata_key.to_string(),
        }
    })?;
    let integrity = match &metadata.resolution {
        LockfileResolution::Tarball(t) => t
            .integrity
            .as_ref()
            .ok_or_else(|| {
                CreateVirtualStoreError::InstallPackageBySnapshot(
                    InstallPackageBySnapshotError::MissingTarballIntegrity {
                        package_key: snapshot_key.to_string(),
                    },
                )
            })?
            .to_string(),
        LockfileResolution::Registry(r) => r.integrity.to_string(),
        LockfileResolution::Directory(_) => {
            return Err(CreateVirtualStoreError::InstallPackageBySnapshot(
                InstallPackageBySnapshotError::UnsupportedResolution {
                    package_key: snapshot_key.to_string(),
                    resolution_kind: "directory",
                },
            ));
        }
        LockfileResolution::Git(_) => {
            return Err(CreateVirtualStoreError::InstallPackageBySnapshot(
                InstallPackageBySnapshotError::UnsupportedResolution {
                    package_key: snapshot_key.to_string(),
                    resolution_kind: "git",
                },
            ));
        }
    };
    let pkg_id = metadata_key.to_string();
    Ok(Some(store_index_key(&integrity, &pkg_id)))
}
