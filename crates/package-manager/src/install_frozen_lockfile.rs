use crate::{
    AllowBuildPolicy, BuildModules, BuildModulesError, CreateVirtualStore, CreateVirtualStoreError,
    CreateVirtualStoreOutput, LinkVirtualStoreBins, LinkVirtualStoreBinsError,
    SymlinkDirectDependencies, SymlinkDirectDependenciesError,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_lockfile::{PackageKey, PackageMetadata, ProjectSnapshot, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_package_manifest::DependencyGroup;
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
            dependency_groups,
            logged_methods,
            requester,
        } = self;

        // TODO: check if the lockfile is out-of-date

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
                logged_methods,
                requester,
                store_index_writer: &store_index_writer,
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
        let allow_build_policy = AllowBuildPolicy::from_manifest(manifest_dir);

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
