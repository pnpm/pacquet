use crate::{CreateVirtualStore, SymlinkDirectDependencies};
use pacquet_lockfile::{DependencyPath, PackageSnapshot, RootProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pacquet_package_json::DependencyGroup;
use pacquet_tarball::Cache;
use reqwest::Client;
use std::collections::HashMap;

#[must_use]
pub struct InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_cache: &'a Cache,
    pub http_client: &'a Client,
    pub config: &'static Npmrc,
    pub project_snapshot: &'a RootProjectSnapshot,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub dependency_groups: DependencyGroupList,
}

impl<'a, DependencyGroupList> InstallFrozenLockfile<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub async fn run(self) {
        let InstallFrozenLockfile {
            tarball_cache,
            http_client,
            config,
            project_snapshot,
            packages,
            dependency_groups,
        } = self;

        // TODO: check if the lockfile is out-of-date

        assert!(config.prefer_frozen_lockfile, "Non frozen lockfile is not yet supported");

        CreateVirtualStore { tarball_cache, http_client, config, packages, project_snapshot }
            .run()
            .await;

        SymlinkDirectDependencies { config, project_snapshot, dependency_groups }.run();
    }
}
