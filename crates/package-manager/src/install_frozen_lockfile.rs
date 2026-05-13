use crate::{
    AllowBuildPolicy, BuildModules, BuildModulesError, CreateVirtualStore, CreateVirtualStoreError,
    CreateVirtualStoreOutput, LinkVirtualStoreBins, LinkVirtualStoreBinsError,
    SymlinkDirectDependencies, SymlinkDirectDependenciesError, VersionPolicyError,
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
    /// Install root, threaded into reporter `requester` fields.
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
            }
            .run::<R>()
            .await
            .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

        // Detect the host `node` major version once per install,
        // not per snapshot. Threaded into `BuildModules` so the
        // side-effects-cache lookup can compose the right cache
        // key. `None` (no `node` on PATH) means the cache gate
        // falls through to "rebuild" — safe.
        //
        // `detect_node_major` spawns `node --version` synchronously,
        // so run it on a blocking thread to keep the async install
        // driver from stalling.
        let engine_name: Option<String> = tokio::task::spawn_blocking(|| {
            pacquet_graph_hasher::detect_node_major()
                .map(|major| pacquet_graph_hasher::engine_name(major, None, None))
        })
        .await
        .ok()
        .flatten();

        SymlinkDirectDependencies { config, importers, dependency_groups, requester }
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

        let manifest_dir = Path::new(requester);

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
