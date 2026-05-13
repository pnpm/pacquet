use crate::{
    InstallPackageBySnapshot, InstallPackageBySnapshotError, SkippedSnapshots,
    store_init::init_store_dir_best_effort,
};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_lockfile::{
    LockfileResolution, PackageKey, PackageMetadata, PkgNameVerPeer, SnapshotEntry,
};
use pacquet_network::ThrottledClient;
use pacquet_reporter::{
    BrokenModulesLog, LogEvent, LogLevel, ProgressLog, ProgressMessage, Reporter, StatsLog,
    StatsMessage,
};
use pacquet_store_dir::{SharedVerifiedFilesCache, StoreIndex, StoreIndexWriter, store_index_key};
use pacquet_tarball::{PrefetchResult, prefetch_cas_paths};
use pipe_trait::Pipe;
use std::{collections::HashMap, path::PathBuf, sync::atomic::AtomicU8};

/// Bundled package manifests recovered from the SQLite store index
/// during [`CreateVirtualStore::run`], keyed by the same
/// `PkgNameVerPeer` (without peer suffix) that
/// [`pacquet_lockfile::Lockfile::packages`] uses. Consumed by the
/// bin-linker so it doesn't have to re-read `package.json` per child
/// during [`crate::LinkVirtualStoreBins::run`].
///
/// Only covers the warm-batch packages (those whose tarball was
/// already in the CAFS at install start). Cold-batch packages — ones
/// pacquet had to download — are absent and the bin linker falls
/// back to disk reads for them. That matches pnpm's behaviour for
/// installs that mix warm and cold packages: pnpm's bin linker
/// reads from `pkgFilesIndex.manifest` for warm fetches and from
/// `dep.fetching()?.bundledManifest` for cold ones, but the cold
/// path's `bundledManifest` isn't plumbed through pacquet yet.
pub type PackageManifests = HashMap<PkgNameVerPeer, std::sync::Arc<serde_json::Value>>;

/// Per-snapshot side-effects-cache overlays, keyed by the snapshot's
/// `PackageKey` and then by the dep-state cache key (the string
/// `pacquet_graph_hasher::calc_dep_state` produces). The inner map
/// is the post-build files map for that cache key — already with
/// the `added` / `deleted` overlay applied against the base files
/// (see `pacquet_store_dir::VerifyResult.side_effects_maps`).
///
/// Multiple snapshot peer-variants of the same package share one
/// `Arc<_>` value — the store-index row is keyed peer-stripped, so
/// each `PackageKey::without_peer()` lookup returns the same
/// underlying map.
///
/// Hands off to `BuildModules`'s `is_built` gate (pnpm/pacquet#421):
/// for a snapshot whose `calc_dep_state` cache key matches an entry
/// here, the build is skipped — pacquet treats the package as
/// already built (typically because pnpm seeded the cache on a
/// previous install).
pub type SideEffectsMapsBySnapshot =
    HashMap<PackageKey, std::sync::Arc<HashMap<String, HashMap<String, PathBuf>>>>;

/// Output of [`CreateVirtualStore::run`]. Bundles the bin-link
/// manifest cache and the per-snapshot side-effects-cache overlays
/// the build-phase needs.
pub struct CreateVirtualStoreOutput {
    pub package_manifests: PackageManifests,
    pub side_effects_maps_by_snapshot: SideEffectsMapsBySnapshot,
}

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Config,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    /// Snapshots and per-version metadata recorded by the previous
    /// install, parsed from `<virtual_store_dir>/lock.yaml`. `None`
    /// on a first install (the file doesn't exist). When present,
    /// per-snapshot lookups against this drive the
    /// `lockfileToDepGraph`-equivalent skip decision — see
    /// [`CreateVirtualStore::run`].
    pub current_snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub current_packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    /// Install-scoped dedupe state for `pnpm:package-import-method`.
    /// See `link_file::log_method_once`.
    pub logged_methods: &'a AtomicU8,
    /// Install root, threaded into reporter `requester` fields.
    pub requester: &'a str,
    /// Shared store-index writer for the install. Owned by
    /// `InstallFrozenLockfile`, threaded down here for the cold-batch
    /// download path's `InstallPackageBySnapshot` and also reused by
    /// `BuildModules` for the side-effects-cache WRITE path.
    pub store_index_writer: &'a std::sync::Arc<StoreIndexWriter>,
    /// `allowBuilds` gate, shared with `BuildModules`. The cold-batch
    /// path threads this into the git fetcher so `preparePackage` can
    /// reject `GIT_DEP_PREPARE_NOT_ALLOWED` for packages that aren't
    /// allowlisted. Computed once per install in
    /// [`crate::InstallFrozenLockfile::run`].
    pub allow_build_policy: &'a crate::AllowBuildPolicy,
    /// Snapshots the installability pass marked optional+incompatible
    /// on this host. Their virtual-store slots are not created — the
    /// warm/cold partition skips them, and the bundled-manifest +
    /// side-effects-cache lookups they would feed downstream phases
    /// are likewise omitted. Mirrors pnpm's `lockfileToDepGraph`
    /// behavior of materializing only non-skipped snapshots in the
    /// graph passed to the build phase.
    pub skipped: &'a SkippedSnapshots,
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
    /// Execute the subroutine. Returns the set of bundled manifests
    /// recovered from `index.db` for the warm-batch slots — the
    /// bin linker uses these to avoid re-reading `package.json` per
    /// child. See [`PackageManifests`].
    pub async fn run<R: Reporter>(
        self,
    ) -> Result<CreateVirtualStoreOutput, CreateVirtualStoreError> {
        let CreateVirtualStore {
            http_client,
            config,
            packages,
            snapshots,
            current_snapshots,
            current_packages,
            logged_methods,
            requester,
            store_index_writer,
            allow_build_policy,
            skipped,
        } = self;

        let Some(snapshots) = snapshots else {
            // No snapshots to install. If the lockfile also has no project deps
            // this is a valid no-op; if it does, pnpm would have populated
            // `snapshots`, so bailing out here is safe enough for v9.
            return Ok(CreateVirtualStoreOutput {
                package_manifests: PackageManifests::new(),
                side_effects_maps_by_snapshot: SideEffectsMapsBySnapshot::new(),
            });
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

        // The batched store-index writer is now owned by the caller
        // (`InstallFrozenLockfile::run`) so it survives past
        // `CreateVirtualStore::run` and gets reused by the build
        // phase's side-effects-cache WRITE path. Pacquet's original
        // pattern was to spawn it here and drain it before returning,
        // but the build phase needs to queue rows after the install
        // path finishes — see pnpm/pnpm@7e3145f9fc:building/during-install/src/index.ts:198-216.
        //
        // The cold-batch download path uses the same writer through
        // `InstallPackageBySnapshot.store_index_writer`, so the design
        // is unchanged from the writer's perspective.
        let store_index_writer_ref = Some(store_index_writer);

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
        // Per-snapshot skip pass: drop snapshots that don't need
        // installing.
        //
        // Two reasons a snapshot can be dropped from the install graph:
        //
        // 1. **Installability skip (this PR)** — `SkippedSnapshots`
        //    contains it because the host's `engines` / `cpu` / `os`
        //    / `libc` don't satisfy the package's constraints and the
        //    snapshot is `optional`. Mirrors pnpm's `lockfileToDepGraph`
        //    behavior at
        //    <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L194>
        //    where skipped depPaths are dropped from the graph the
        //    builder iterates. These snapshots also stay out of the
        //    `skipped_entries` cache-key pass — they were never
        //    supposed to be installed, so there are no store-index
        //    rows to keep alive.
        //
        // 2. **Current-lockfile skip (main #442)** — the previous
        //    install also installed this snapshot (`current_snapshots`)
        //    with the same dependency wiring + integrity, AND its
        //    virtual-store slot still exists on disk. Mirrors
        //    upstream's gate at
        //    <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L246-L260>.
        //    These DO land in `skipped_entries` so `BuildModules`'s
        //    `is_built` cache lookup can short-circuit re-runs of
        //    allowed-build scripts on warm reinstalls.
        //
        // Run this *before* deriving cache keys so unchanged
        // directory-backed snapshots aren't tripped by
        // `snapshot_cache_key`'s `UnsupportedResolution`.
        let virtual_store_dir = &config.virtual_store_dir;
        let survivors: Vec<(&PackageKey, &SnapshotEntry)> = snapshots
            .iter()
            // Reason 1: installability skip. Drop entirely.
            .filter(|(snapshot_key, _)| !skipped.contains(snapshot_key))
            // Reason 2: current-lockfile skip. Drop survivors that
            // already match the previous install.
            .filter(|(snapshot_key, snapshot)| {
                let Some(current_snapshots) = current_snapshots else { return true };
                let Some(current_snapshot) = current_snapshots.get(*snapshot_key) else {
                    return true;
                };
                if !snapshot_deps_equal(current_snapshot, snapshot) {
                    return true;
                }
                let current_metadata =
                    current_packages.and_then(|p| p.get(&snapshot_key.without_peer()));
                let wanted_metadata = packages.get(&snapshot_key.without_peer());
                if !integrity_equal(current_metadata, wanted_metadata) {
                    return true;
                }
                let dir = virtual_store_dir
                    .join(snapshot_key.to_virtual_store_name())
                    .join("node_modules")
                    .join(snapshot_key.name.to_string());
                if dir.is_dir() {
                    false
                } else {
                    R::emit(&LogEvent::BrokenModules(BrokenModulesLog {
                        level: LogLevel::Debug,
                        missing: dir.to_string_lossy().into_owned(),
                    }));
                    true
                }
            })
            .collect();

        // Validate every surviving snapshot upfront so a malformed
        // lockfile (missing metadata, missing tarball integrity,
        // currently-unsupported directory / git resolution) errors
        // out *before* we start the warm batch. Previously we
        // collapsed those cases into `None` and let them fall through
        // to the cold batch, which meant the warm rayon batch ran to
        // completion (~6 s on `alot7`) before the actual error fired.
        //
        // Cache-key derivation runs in two passes:
        //
        // - *Survivors* go through the strict path (this `?`). Their
        //   resolutions have to be valid because the install will
        //   actually fetch + link them.
        // - *Skipped* snapshots get a lenient pass below: cache keys
        //   are derived if possible, and any per-snapshot error is
        //   swallowed. Reason: skipped snapshots aren't being
        //   re-installed, but their store-index rows still need to
        //   land in `side_effects_maps_by_snapshot` so
        //   [`crate::BuildModules`]'s `is_built` gate can skip
        //   re-running build scripts on warm reinstalls (review on
        //   #442 — without this, allowed-build packages re-execute
        //   their scripts every install, costing seconds on the
        //   warm-reinstall path).
        type SnapshotWithCacheKey<'a> = (&'a PackageKey, &'a SnapshotEntry, Option<String>);
        let snapshot_entries: Vec<SnapshotWithCacheKey<'_>> = survivors
            .into_iter()
            .map(|(snapshot_key, snapshot)| {
                snapshot_cache_key(snapshot_key, packages).map(|key| (snapshot_key, snapshot, key))
            })
            .collect::<Result<_, _>>()?;

        // Cache keys for the *skipped* snapshots (i.e. snapshots
        // present in `snapshots` but absent from `snapshot_entries`).
        // Derived leniently so an unsupported / malformed skipped
        // entry doesn't fail the install — it just contributes no
        // prefetch row, which is the same outcome as if the skip
        // filter had not engaged. Built as a parallel `Vec` so the
        // downstream `package_manifests` /
        // `side_effects_maps_by_snapshot` loop sees the full snapshot
        // set, not just survivors.
        let survivor_keys: std::collections::HashSet<&PackageKey> =
            snapshot_entries.iter().map(|(k, _, _)| *k).collect();
        let skipped_entries: Vec<SnapshotWithCacheKey<'_>> = snapshots
            .iter()
            .filter(|(snapshot_key, _)| !survivor_keys.contains(snapshot_key))
            // Installability-skipped snapshots are excluded from
            // `skipped_entries` too — they were never installed, so
            // there's no store-index row to keep warm for the
            // build-cache lookup. Only the current-lockfile-skip
            // path (`survivors` filtered above) should contribute
            // here.
            .filter(|(snapshot_key, _)| !skipped.contains(snapshot_key))
            .map(|(snapshot_key, snapshot)| {
                let cache_key = snapshot_cache_key(snapshot_key, packages).ok().flatten();
                (snapshot_key, snapshot, cache_key)
            })
            .collect();

        // `pnpm:stats added` mirrors pnpm's emit at
        // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/link.ts#L363>:
        // one event per project once the orchestrator has decided
        // how many packages will land in the virtual store. Upstream
        // reports `newDepPathsSet.size`, the *delta* between current
        // and wanted lockfile; pacquet computes the same delta as the
        // post-skip-filter snapshot count so a warm reinstall against
        // an unchanged lockfile reports `added: 0`.
        //
        // `pnpm:stats removed: 0` mirrors the no-current-lockfile
        // branch of
        // <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-restorer/src/index.ts#L290>:
        // pnpm emits a placeholder `0` when there's nothing to prune
        // so consumers don't render a stale "removed" count from a
        // previous install. Pacquet has no pruning pipeline yet, so
        // the placeholder is the truthful value today.
        R::emit(&LogEvent::Stats(StatsLog {
            level: LogLevel::Debug,
            message: StatsMessage::Added {
                prefix: requester.to_owned(),
                added: snapshot_entries.len() as u64,
            },
        }));
        R::emit(&LogEvent::Stats(StatsLog {
            level: LogLevel::Debug,
            message: StatsMessage::Removed { prefix: requester.to_owned(), removed: 0 },
        }));

        // Union the cache keys from survivors and skipped snapshots
        // so the prefetch covers everyone the build phase might need
        // to gate on. Sorted + deduplicated to avoid redundant SQL
        // queries in `prefetch_cas_paths`.
        let mut cache_key_refs: Vec<&str> = snapshot_entries
            .iter()
            .chain(skipped_entries.iter())
            .filter_map(|(_, _, k)| k.as_deref())
            .collect();
        cache_key_refs.sort_unstable();
        cache_key_refs.dedup();
        let cache_keys: Vec<String> = cache_key_refs.into_iter().map(String::from).collect();
        let PrefetchResult {
            cas_paths: prefetched,
            manifests: prefetched_manifests,
            side_effects_maps: prefetched_side_effects,
        } = prefetch_cas_paths(
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
        // Build a `metadata_key -> manifest` lookup from the prefetched
        // index rows. Snapshot keys differ across peer-resolved
        // variants of the same package (`react-dom@17.0.2(react@...)`),
        // but the bundled manifest is identical across variants
        // because every variant resolves to the same tarball. Keying
        // by [`PkgNameVerPeer::without_peer`] collapses the variants
        // to one entry: same shape as
        // [`pacquet_lockfile::Lockfile::packages`], which is what the
        // bin linker already looks up by.
        let mut package_manifests: PackageManifests =
            HashMap::with_capacity(prefetched_manifests.len());
        let mut side_effects_maps_by_snapshot: SideEffectsMapsBySnapshot =
            HashMap::with_capacity(prefetched_side_effects.len());

        // First pass: process *skipped* snapshots into the bin-
        // manifest cache and the side-effects map. They don't enter
        // the warm/cold partition (no link work to do), but their
        // store-index rows are needed downstream so
        // [`crate::BuildModules`]'s `is_built` gate can fire — without
        // these entries, packages with `allowBuilds: true` would
        // re-execute their lifecycle scripts on every warm reinstall.
        for (snapshot_key, _snapshot, cache_key) in &skipped_entries {
            if let Some(cache_key) = cache_key.as_deref()
                && let Some(manifest) = prefetched_manifests.get(cache_key)
            {
                package_manifests
                    .entry(snapshot_key.without_peer())
                    .or_insert_with(|| std::sync::Arc::clone(manifest));
            }
            if let Some(cache_key) = cache_key.as_deref()
                && let Some(maps) = prefetched_side_effects.get(cache_key)
            {
                side_effects_maps_by_snapshot
                    .insert((*snapshot_key).clone(), std::sync::Arc::clone(maps));
            }
        }

        // Second pass: survivors. Same loop as above plus the
        // warm/cold partition that decides which snapshots run the
        // link work.
        for (snapshot_key, snapshot, cache_key) in &snapshot_entries {
            if let Some(cache_key) = cache_key.as_deref()
                && let Some(manifest) = prefetched_manifests.get(cache_key)
            {
                package_manifests
                    .entry(snapshot_key.without_peer())
                    .or_insert_with(|| std::sync::Arc::clone(manifest));
            }
            // Peer-variants of the same package share the same
            // store-index row → the same `Arc<_>`. Cheap to share.
            if let Some(cache_key) = cache_key.as_deref()
                && let Some(maps) = prefetched_side_effects.get(cache_key)
            {
                side_effects_maps_by_snapshot
                    .insert((*snapshot_key).clone(), std::sync::Arc::clone(maps));
            }
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
                    emit_warm_snapshot_progress::<R>(&package_id, requester);

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
                        allow_build_policy,
                    }
                    .run::<R>()
                    .await
                    .map_err(CreateVirtualStoreError::InstallPackageBySnapshot)
                })
                .pipe(future::try_join_all)
                .await?;
        }

        // The writer is owned by the caller now. They drop their
        // sender and await the join handle after the build phase
        // finishes, so the final batch flushes after every queued
        // row from both the download path and the WRITE-path
        // upload.

        Ok(CreateVirtualStoreOutput { package_manifests, side_effects_maps_by_snapshot })
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
            // Git resolutions don't participate in the warm prefetch
            // batch in this PR — the fetcher decides the
            // `gitHostedStoreIndexKey` `built` bit at fetch time
            // (`preparePackage` returns `shouldBeBuilt`) and writes
            // no store-index row, so there's nothing for the warm
            // path to read back yet. Returning `Ok(None)` routes the
            // snapshot through the cold-batch
            // [`InstallPackageBySnapshot`], where
            // [`pacquet_git_fetcher::GitFetcher`] handles the
            // clone + checkout + import. Wiring warm caching for
            // git-hosted entries is a follow-up tracked alongside
            // Section C (the git-hosted *tarball* path) in #436.
            return Ok(None);
        }
    };
    let pkg_id = metadata_key.to_string();
    Ok(Some(store_index_key(&integrity, &pkg_id)))
}

/// Two snapshots agree on dependency wiring when both their
/// `dependencies` and `optionalDependencies` maps are equal in
/// upstream's sense — an absent map and an empty map are equivalent
/// (`equals({}, undefined)` and `isEmpty({}) === isEmpty(undefined)`
/// both hold in Ramda). Mirrors the AND-pair in
/// <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L246-L260>:
/// the deps check is the `depIsPresent && equals(...)` arm and the
/// optional-deps check is the `isEmpty(...) && isEmpty(...) ||
/// equals(...)` arm folded together.
fn snapshot_deps_equal(current: &SnapshotEntry, wanted: &SnapshotEntry) -> bool {
    fn maps_equal<K, V>(a: Option<&HashMap<K, V>>, b: Option<&HashMap<K, V>>) -> bool
    where
        K: std::cmp::Eq + std::hash::Hash,
        V: PartialEq,
    {
        match (a, b) {
            (None, None) => true,
            (Some(m), None) | (None, Some(m)) => m.is_empty(),
            (Some(x), Some(y)) => x == y,
        }
    }
    maps_equal(current.dependencies.as_ref(), wanted.dependencies.as_ref())
        && maps_equal(current.optional_dependencies.as_ref(), wanted.optional_dependencies.as_ref())
}

/// Compare the `integrity` field on two `packages:` entries. Mirrors
/// upstream's `isIntegrityEqual` helper at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L366>:
/// only the tarball/registry-style integrity participates in the
/// check; directory and git resolutions yield `None` on both sides,
/// which we treat as "unchanged" so the existing slot is reused.
fn integrity_equal(current: Option<&PackageMetadata>, wanted: Option<&PackageMetadata>) -> bool {
    let current_integrity = current.and_then(|m| m.resolution.integrity());
    let wanted_integrity = wanted.and_then(|m| m.resolution.integrity());
    current_integrity == wanted_integrity
}

/// `pnpm:progress` `resolved` + `found_in_store` for a frozen-lockfile
/// snapshot the install-scoped prefetch already settled. Frozen-
/// lockfile snapshots are "already resolved" by virtue of being in
/// the lockfile, and "found in store" by virtue of the prefetch
/// covering them — emit both so the consumer sees the full resolved
/// → found_in_store → imported sequence even when the cold path is
/// skipped.
///
/// Pulled out of the warm-batch closure in
/// [`CreateVirtualStore::run`] so the event-construction code is
/// unit-testable; the call site stays in the warm-batch hot path
/// where setting up a non-empty prefetched-cas test would require a
/// full lockfile + populated CAFS.
fn emit_warm_snapshot_progress<R: Reporter>(package_id: &str, requester: &str) {
    R::emit(&LogEvent::Progress(ProgressLog {
        level: LogLevel::Debug,
        message: ProgressMessage::Resolved {
            package_id: package_id.to_owned(),
            requester: requester.to_owned(),
        },
    }));
    R::emit(&LogEvent::Progress(ProgressLog {
        level: LogLevel::Debug,
        message: ProgressMessage::FoundInStore {
            package_id: package_id.to_owned(),
            requester: requester.to_owned(),
        },
    }));
}

#[cfg(test)]
mod tests;
