use crate::InstallPackageBySnapshot;
use futures_util::future;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, RootProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pipe_trait::Pipe;
use reqwest::Client;
use std::collections::HashMap;

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a Client,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub project_snapshot: &'a RootProjectSnapshot,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) {
        let CreateVirtualStore { http_client, config, packages, project_snapshot } = self;

        let packages = packages.unwrap_or_else(|| {
            dbg!(project_snapshot);
            todo!("check project_snapshot, error if it's not empty, do nothing if empty");
        });

        packages
            .iter()
            .map(|(dependency_path, package_snapshot)| async move {
                InstallPackageBySnapshot { http_client, config, dependency_path, package_snapshot }
                    .run()
                    .await
                    .unwrap(); // TODO: properly propagate this error
            })
            .pipe(future::join_all)
            .await;
    }
}
