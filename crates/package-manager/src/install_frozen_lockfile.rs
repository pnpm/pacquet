use crate::{CreateVirtualStore, CreateVirtualStoreError, SymlinkDirectDependencies};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, RootProjectSnapshot};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use std::collections::HashMap;

/// This subroutine installs dependencies from a frozen lockfile.
///
/// **Brief overview:**
/// * Iterate over each package in [`Self::packages`].
/// * Fetch a tarball of each package.
/// * Extract each tarball into the store directory.
/// * Import (by reflink, hardlink, or copy) the files from the store dir to each `node_modules/.pacquet/{name}@{version}/node_modules/{name}/`.
/// * Create dependency symbolic links in each `node_modules/.pacquet/{name}@{version}/node_modules/`.
/// * Create a symbolic link at each `node_modules/{name}`.
#[must_use]
pub struct InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub project_snapshot: &'a RootProjectSnapshot,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub dependency_groups: DependencyGroupList,
}

/// Error type of [`InstallFrozenLockfile`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallFrozenLockfileError {
    #[diagnostic(transparent)]
    CreateVirtualStore(#[error(source)] CreateVirtualStoreError),
}

impl<'a, DependencyGroupList> InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), InstallFrozenLockfileError> {
        let InstallFrozenLockfile {
            http_client,
            config,
            project_snapshot,
            packages,
            dependency_groups,
        } = self;

        // TODO: check if the lockfile is out-of-date

        assert!(config.prefer_frozen_lockfile, "Non frozen lockfile is not yet supported");

        CreateVirtualStore { http_client, config, packages, project_snapshot }
            .run()
            .await
            .map_err(InstallFrozenLockfileError::CreateVirtualStore)?;

        SymlinkDirectDependencies { config, project_snapshot, dependency_groups }.run();

        Ok(())
    }
}
