use crate::{
    CreateVirtualStore, CreateVirtualStoreError, LinkVirtualStoreBins, LinkVirtualStoreBinsError,
    SymlinkDirectDependencies, SymlinkDirectDependenciesError,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{PackageKey, PackageMetadata, ProjectSnapshot, SnapshotEntry};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use pacquet_reporter::Reporter;
use std::{collections::HashMap, sync::atomic::AtomicU8};

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
    pub config: &'static Npmrc,
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

        CreateVirtualStore { http_client, config, packages, snapshots, logged_methods, requester }
            .run::<R>()
            .await
            .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

        SymlinkDirectDependencies { config, importers, dependency_groups }
            .run()
            .map_err(InstallFrozenLockfileError::SymlinkDirectDependencies)?;

        // Link the bins of each virtual-store slot's children into the
        // slot's own `node_modules/.bin`. Pnpm runs this from
        // `linkBinsOfDependencies` during the headless install. See
        // <https://github.com/pnpm/pnpm/blob/4750fd370c/building/during-install/src/index.ts#L258-L309>.
        LinkVirtualStoreBins { virtual_store_dir: &config.virtual_store_dir }
            .run()
            .map_err(InstallFrozenLockfileError::LinkVirtualStoreBins)?;

        Ok(())
    }
}
