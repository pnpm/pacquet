use crate::{
    AllowBuildPolicy, BuildModules, BuildModulesError, CreateVirtualStore, CreateVirtualStoreError,
    LinkVirtualStoreBins, LinkVirtualStoreBinsError, SymlinkDirectDependencies,
    SymlinkDirectDependenciesError,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_config::Config;
use pacquet_lockfile::{PackageKey, PackageMetadata, ProjectSnapshot, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::{IgnoredScriptsLog, LogEvent, LogLevel, Reporter, Stage, StageLog};
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

        let package_manifests = CreateVirtualStore {
            http_client,
            config,
            packages,
            snapshots,
            logged_methods,
            requester,
        }
        .run::<R>()
        .await
        .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

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
            importers,
            allow_build_policy: &allow_build_policy,
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

        Ok(())
    }
}
