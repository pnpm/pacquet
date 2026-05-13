use crate::{
    AllowBuildPolicy, BuildModules, BuildModulesError, CreateVirtualStore, CreateVirtualStoreError,
    CreateVirtualStoreOutput, InstallabilityHost, LinkVirtualStoreBins, LinkVirtualStoreBinsError,
    SkippedSnapshots, SymlinkDirectDependencies, SymlinkDirectDependenciesError,
    VersionPolicyError, any_installability_constraint, compute_skipped_snapshots,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_executor::ScriptsPrependNodePath as ExecScriptsPrependNodePath;
use pacquet_lockfile::{PackageKey, PackageMetadata, ProjectSnapshot, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_package_manifest::DependencyGroup;
use pacquet_patching::{
    ExtendedPatchInfo, PatchKeyConflictError, ResolvePatchedDependenciesError, get_patch_info,
};
use pacquet_reporter::{IgnoredScriptsLog, LogEvent, LogLevel, Reporter, Stage, StageLog};
use pacquet_store_dir::StoreIndexWriter;
use std::{collections::HashMap, path::Path, sync::atomic::AtomicU8};

/// This subroutine installs dependencies from a frozen lockfile.
///
/// **Brief overview:**
/// * Iterate over each snapshot in the v9 `snapshots:` map.
/// * Fetch the tarball for the matching `packages:` entry.
/// * Extract each tarball into the store directory.
/// * Import the files from the store dir to each `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in each `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at each `node_modules/{name}`.
#[must_use]
pub struct InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub http_client: &'a ThrottledClient,
    pub config: &'static Config,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    /// Snapshots from the previous install's `lock.yaml`, if present.
    /// Threaded through to [`crate::CreateVirtualStore`] to drive the
    /// per-snapshot skip decision (a snapshot whose wiring and
    /// integrity haven't changed and whose virtual-store slot still
    /// exists on disk is dropped from the install graph). `None` on a
    /// first install — the current-lockfile file doesn't exist yet.
    pub current_snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub current_packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub dependency_groups: DependencyGroupList,
    /// Install-scoped dedupe state for `pnpm:package-import-method`.
    /// See `link_file::log_method_once`.
    pub logged_methods: &'a AtomicU8,
    /// Install root — the directory containing `pnpm-lock.yaml`.
    /// For a real workspace, this is the workspace root (the dir
    /// containing `pnpm-workspace.yaml`); for a single-project
    /// install, it's the project dir.
    ///
    /// Reporter envelopes (`pnpm:stage`, `pnpm:summary`, `pnpm:lifecycle`)
    /// use [`requester`], a lossy-UTF-8 string view of this path —
    /// per-importer events like `pnpm:root` use the importer's own
    /// `rootDir` instead. Filesystem operations that need the real
    /// path (the per-importer `node_modules/` write under
    /// `SymlinkDirectDependencies`, the `lockfile_dir` threaded into
    /// `BuildModules`) use `workspace_root` directly so the round-trip
    /// through a lossy string can never corrupt the on-disk path on
    /// hosts with non-UTF-8 filenames.
    ///
    /// [`requester`]: Self::requester
    pub workspace_root: &'a Path,

    /// Lossy-UTF-8 view of [`workspace_root`] for reporter envelopes.
    /// Kept as a separate field rather than recomputed from
    /// `workspace_root` so the caller controls how the conversion is
    /// performed (today: `to_string_lossy().into_owned()` in
    /// `Install::run`).
    ///
    /// [`workspace_root`]: Self::workspace_root
    pub requester: &'a str,
}

/// Error type of [`InstallFrozenLockfile`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallFrozenLockfileError {
    #[diagnostic(transparent)]
    CreateVirtualStore(#[error(source)] CreateVirtualStoreError),

    #[diagnostic(transparent)]
    SymlinkDirectDependencies(#[error(source)] SymlinkDirectDependenciesError),

    #[diagnostic(transparent)]
    LinkVirtualStoreBins(#[error(source)] LinkVirtualStoreBinsError),

    #[diagnostic(transparent)]
    BuildModules(#[error(source)] BuildModulesError),

    #[diagnostic(transparent)]
    ResolvePatchedDependencies(#[error(source)] ResolvePatchedDependenciesError),

    /// Surfaces upstream's `ERR_PNPM_PATCH_KEY_CONFLICT` when more
    /// than one configured version range matches a snapshot. Mirrors
    /// pnpm's behavior of refusing to silently pick one — the user
    /// must add an exact-version entry to disambiguate. See
    /// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/patching/config/src/getPatchInfo.ts#L5-L19>.
    #[diagnostic(transparent)]
    PatchKeyConflict(#[error(source)] PatchKeyConflictError),

    /// Surfaces upstream's `ERR_PNPM_INVALID_VERSION_UNION` /
    /// `ERR_PNPM_NAME_PATTERN_IN_VERSION_UNION` when an
    /// `allowBuilds` key in `pnpm-workspace.yaml` can't be parsed.
    /// See <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/version-policy/src/index.ts#L60-L80>.
    #[diagnostic(transparent)]
    VersionPolicy(#[error(source)] VersionPolicyError),

    /// Wraps any error `compute_skipped_snapshots` surfaces from the
    /// installability pass. Three sources, all reachable under
    /// today's default config:
    ///
    /// - `InstallabilityError::InvalidNodeVersion` — the resolved
    ///   `current_node_version` isn't a parseable exact semver.
    ///   Pacquet falls back to a synthetic `99999.0.0` when
    ///   `node --version` fails, so this is currently unreachable
    ///   from production — but a future `nodeVersion` config wiring
    ///   (slice 2) will surface user-supplied bad values here,
    ///   mirroring upstream's `ERR_PNPM_INVALID_NODE_VERSION` throw
    ///   at <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/checkEngine.ts#L25-L27>.
    /// - `InstallabilityError::Engine` / `InstallabilityError::Platform`
    ///   from a non-optional incompatible snapshot with
    ///   `engine_strict = true`. Pacquet's default has
    ///   `engine_strict = false`, so this path is currently
    ///   unreachable from production either — wired through so the
    ///   slice that lands the config setting doesn't churn the
    ///   error enum again. Mirrors upstream's `throw warn` at
    ///   <https://github.com/pnpm/pnpm/blob/94240bc046/config/package-is-installable/src/index.ts#L63>.
    #[diagnostic(transparent)]
    Installability(#[error(source)] Box<pacquet_package_is_installable::InstallabilityError>),
}

impl<'a, DependencyGroupList> InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run<R: Reporter>(self) -> Result<(), InstallFrozenLockfileError> {
        let InstallFrozenLockfile {
            http_client,
            config,
            importers,
            packages,
            snapshots,
            current_snapshots,
            current_packages,
            dependency_groups,
            logged_methods,
            workspace_root,
            requester,
        } = self;

        // TODO: check if the lockfile is out-of-date

        // Build the allow-builds policy up front so it can flow into
        // the cold-batch git fetcher in `CreateVirtualStore` as well as
        // the postinstall phase in `BuildModules`. Mirrors pnpm where
        // `createAllowBuildFunction` is a per-install constant.
        let allow_build_policy = AllowBuildPolicy::from_config(config)
            .map_err(InstallFrozenLockfileError::VersionPolicy)?;

        // Spawn the batched store-index writer here so it lives
        // across both the prefetch/download phase (consumers in
        // `CreateVirtualStore`) and the build phase (the new
        // side-effects-cache WRITE-path upload site in
        // `BuildModules`). We drop the orchestrator's clone and
        // await the join handle at the end of `run`, so the final
        // batch flushes once every queued row from both phases has
        // been processed. A writer open / task failure is degraded
        // to a `warn!` and the install still succeeds — pacquet's
        // existing best-effort stance on cache writes.
        let (store_index_writer, writer_task) = StoreIndexWriter::spawn(&config.store_dir);

        // Caller-side fast-path for the installability check. The
        // common case (no lockfile metadata row declares an
        // `engines` / `cpu` / `os` / `libc` constraint) lets us skip
        // both [`InstallabilityHost::detect`] and
        // [`compute_skipped_snapshots`] entirely. Spawning
        // `node --version` here would otherwise serialize the
        // node-binary startup with `CreateVirtualStore::run` (the
        // dominant cost of a cold install), giving up the overlap
        // pacquet had before — see the previous benchmark regression
        // on this PR.
        //
        // When constraints DO exist, the host is needed before
        // extraction (so `CreateVirtualStore` can suppress slots for
        // skipped snapshots), and the spawn cost is unavoidable.
        let needs_installability_check = match (snapshots, packages) {
            (Some(snaps), Some(pkgs)) if !snaps.is_empty() => any_installability_constraint(pkgs),
            _ => false,
        };

        // Build the per-install [`SkippedSnapshots`] set. For every
        // lockfile snapshot, run the installability check against
        // the host triple; optional+incompatible entries land in
        // the set and fire `pnpm:skipped-optional-dependency`.
        // Mirrors pnpm's headless re-check at
        // <https://github.com/pnpm/pnpm/blob/94240bc046/deps/graph-builder/src/lockfileToDepGraph.ts#L206-L215>.
        //
        // `host` is built only when needed. The detection path runs
        // `node --version` on the blocking pool so it doesn't stall
        // the reactor thread.
        let (skipped, host_node) = if needs_installability_check {
            let host = tokio::task::spawn_blocking(InstallabilityHost::detect)
                .await
                .unwrap_or_else(|_| InstallabilityHost {
                    node_version: "99999.0.0".to_string(),
                    node_detected: false,
                    os: pacquet_graph_hasher::host_platform(),
                    cpu: pacquet_graph_hasher::host_arch(),
                    libc: pacquet_graph_hasher::host_libc(),
                    supported_architectures: None,
                    engine_strict: false,
                });
            let s = compute_skipped_snapshots::<R>(
                snapshots.expect("guarded by needs_installability_check"),
                packages.expect("guarded by needs_installability_check"),
                &host,
                requester,
            )
            .map_err(InstallFrozenLockfileError::Installability)?;
            // Preserve `node_detected` + `node_version` for the
            // engine-name derivation below. Dropping the rest of the
            // host struct frees the allocations early.
            (s, Some((host.node_detected, host.node_version)))
        } else {
            (SkippedSnapshots::new(), None)
        };

        let CreateVirtualStoreOutput { package_manifests, side_effects_maps_by_snapshot } =
            CreateVirtualStore {
                http_client,
                config,
                packages,
                snapshots,
                current_snapshots,
                current_packages,
                logged_methods,
                requester,
                store_index_writer: &store_index_writer,
                allow_build_policy: &allow_build_policy,
                skipped: &skipped,
            }
            .run::<R>()
            .await
            .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

        // `engine_name` for the side-effects-cache lookup.
        //
        // Two paths:
        // - We already detected the host for the installability
        //   check (constraint-bearing lockfile): reuse the cached
        //   version. The synthetic-fallback case (`node_detected = false`)
        //   yields `None` so a bogus `99999.0.0`-derived key can't
        //   poison the cache.
        // - We skipped the installability check (constraint-free
        //   lockfile, the common case): no cached version. Fall
        //   back to the legacy `detect_node_major` spawn — run
        //   after `CreateVirtualStore::run` so it overlaps with
        //   nothing on the critical path. This is the same
        //   placement upstream used to have.
        let engine_name: Option<String> = match &host_node {
            Some((true, ver)) => parse_major_from_version(ver)
                .map(|major| pacquet_graph_hasher::engine_name(major, None, None)),
            Some((false, _)) => None,
            None => tokio::task::spawn_blocking(|| {
                pacquet_graph_hasher::detect_node_major()
                    .map(|major| pacquet_graph_hasher::engine_name(major, None, None))
            })
            .await
            .ok()
            .flatten(),
        };

        SymlinkDirectDependencies {
            config,
            importers,
            dependency_groups,
            workspace_root,
            skipped: &skipped,
        }
        .run::<R>()
        .map_err(InstallFrozenLockfileError::SymlinkDirectDependencies)?;

        // Link the bins of each virtual-store slot's children into the
        // slot's own `node_modules/.bin`. Pnpm runs this from
        // `linkBinsOfDependencies` during the headless install. See
        // <https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L258-L309>.
        // Done before `importing_done` so reporters see the import phase
        // close only after every link (including per-slot bins) is in
        // place. The manifest map threaded from `CreateVirtualStore`
        // lets the linker hit `pkgFilesIndex.manifest` directly
        // (matching pnpm's `bundledManifest`-from-CAFS path) instead
        // of re-reading every child's `package.json` from disk.
        LinkVirtualStoreBins {
            virtual_store_dir: &config.virtual_store_dir,
            snapshots,
            packages,
            package_manifests: &package_manifests,
            skipped: &skipped,
        }
        .run()
        .map_err(InstallFrozenLockfileError::LinkVirtualStoreBins)?;

        // Mirrors upstream `link.ts:167-170`: `importing_done` fires once
        // extraction and symlink linking are complete, before any build
        // phase. Reporters use it to close the import progress display so
        // subsequent `pnpm:lifecycle` events render in their own section.
        // <https://github.com/pnpm/pnpm/blob/80037699fb/installing/deps-installer/src/install/link.ts#L167>
        R::emit(&LogEvent::Stage(StageLog {
            level: LogLevel::Debug,
            prefix: requester.to_string(),
            stage: Stage::ImportingDone,
        }));

        // `manifest_dir` (= upstream's `lockfileDir`) is the workspace
        // root threaded through `BuildModules`. Use the real `Path`
        // here rather than reconstructing it from the lossy
        // `requester` string so non-UTF-8 filenames survive intact.
        // `allow_build_policy` was already constructed up-front
        // (before `CreateVirtualStore`) on `main` so the git fetcher
        // can consult it — no second construction needed here.
        let manifest_dir: &Path = workspace_root;

        // Resolve `pnpm-workspace.yaml`'s `patchedDependencies` once
        // per install. Yields `None` when nothing is configured (no
        // yaml, no key, or empty map). Mirrors upstream's single
        // `calcPatchHashes` + `groupPatchedDependencies` call at
        // <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/src/install/index.ts#L468-L488>.
        let patch_groups = config
            .resolved_patched_dependencies()
            .map_err(InstallFrozenLockfileError::ResolvePatchedDependencies)?;

        // Look every snapshot up against the resolved record and
        // build a per-snapshot map keyed by the peer-stripped
        // `PackageKey` (patches are configured at name+version
        // granularity, not per peer-resolution variant). `None` when
        // no patches are configured at all; an empty map when patches
        // exist but match nothing in the current install.
        //
        // Mirrors upstream's per-node lookup at
        // <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/pkg-manager/resolve-dependencies/src/resolveDependencies.ts#L1482>,
        // adapted for pacquet's lockfile-driven flow: pnpm computes
        // `node.patch` during resolution, pacquet computes it after
        // lockfile load.
        let patches: Option<HashMap<PackageKey, ExtendedPatchInfo>> =
            match (patch_groups.as_ref(), snapshots) {
                (Some(groups), Some(snaps)) => {
                    let mut map = HashMap::new();
                    for key in snaps.keys() {
                        let metadata_key = key.without_peer();
                        let metadata_key_str = metadata_key.to_string();
                        let (name, version) =
                            crate::build_modules::parse_name_version_from_key(&metadata_key_str);
                        // Propagate `ERR_PNPM_PATCH_KEY_CONFLICT` rather
                        // than silently skipping the snapshot. Upstream
                        // fails the install here so the user adds an
                        // exact-version entry to disambiguate — silently
                        // dropping the patch would leave the package
                        // unpatched (and the cache key unchanged) without
                        // any signal.
                        if let Some(info) = get_patch_info(Some(groups), &name, &version)
                            .map_err(InstallFrozenLockfileError::PatchKeyConflict)?
                        {
                            map.insert(metadata_key, info.clone());
                        }
                    }
                    Some(map)
                }
                _ => None,
            };

        // Convert `pacquet-config`'s mirror enum to the executor's
        // canonical type. Config's enum carries the yaml-deserialize
        // impl; the executor's stays free of serde wiring. See the
        // doc on [`pacquet_config::ScriptsPrependNodePath`] for the
        // rationale.
        let scripts_prepend_node_path = match config.scripts_prepend_node_path {
            pacquet_config::ScriptsPrependNodePath::Always => ExecScriptsPrependNodePath::Always,
            pacquet_config::ScriptsPrependNodePath::Never => ExecScriptsPrependNodePath::Never,
            pacquet_config::ScriptsPrependNodePath::WarnOnly => {
                ExecScriptsPrependNodePath::WarnOnly
            }
        };

        let ignored_builds = BuildModules {
            virtual_store_dir: &config.virtual_store_dir,
            modules_dir: &config.modules_dir,
            lockfile_dir: manifest_dir,
            snapshots,
            packages,
            importers,
            allow_build_policy: &allow_build_policy,
            side_effects_maps_by_snapshot: Some(&side_effects_maps_by_snapshot),
            engine_name: engine_name.as_deref(),
            side_effects_cache: config.side_effects_cache_read(),
            side_effects_cache_write: config.side_effects_cache_write(),
            store_dir: Some(&config.store_dir),
            store_index_writer: Some(&store_index_writer),
            patches: patches.as_ref(),
            scripts_prepend_node_path,
            unsafe_perm: config.unsafe_perm,
            child_concurrency: config.child_concurrency,
            skipped: &skipped,
        }
        .run::<R>()
        .map_err(InstallFrozenLockfileError::BuildModules)?;

        // Mirrors upstream's single emit at the end of the build phase:
        // <https://github.com/pnpm/pnpm/blob/80037699fb/installing/deps-installer/src/install/index.ts#L414>.
        // Always emitted (with an empty list when nothing was ignored), so
        // the reporter can display a consistent "no ignored scripts" state.
        R::emit(&LogEvent::IgnoredScripts(IgnoredScriptsLog {
            level: LogLevel::Debug,
            package_names: ignored_builds,
        }));

        // Drop the orchestrator's clone of the writer so the channel
        // closes once every per-snapshot clone has also been dropped;
        // then await the task so the final batch flushes before
        // returning. Swallow any error with `warn!` — the install is
        // complete and a missed cache write just forces a re-fetch
        // on the next install.
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

/// Pull the leading major-version digits out of a semver string like
/// `"22.11.0"`. Returns `None` if the leading token isn't parseable
/// as `u32`. Used to derive the engine-name string upstream's
/// side-effects cache lookup expects without re-spawning
/// `node --version`.
fn parse_major_from_version(version: &str) -> Option<u32> {
    let after_v = version.strip_prefix('v').unwrap_or(version);
    after_v.split('.').next()?.parse().ok()
}
