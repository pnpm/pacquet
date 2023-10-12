use crate::InstallPackageBySnapshot;
use futures_util::future;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, RootProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pacquet_tarball::Cache;
use pipe_trait::Pipe;
use reqwest::Client;
use std::collections::HashMap;

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Configuration read from `.npmrc`.
    pub config: &'static Npmrc,
    /// The `packages` object from the lockfile.
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    /// The part of the lockfile that snapshots `package.json`.
    pub project_snapshot: &'a RootProjectSnapshot,
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn create(self) {
        let CreateVirtualStore { tarball_cache, http_client, config, packages, project_snapshot } =
            self;

        let packages = packages.unwrap_or_else(|| {
            dbg!(project_snapshot);
            todo!("check project_snapshot, error if it's not empty, do nothing if empty");
        });

        packages
            .iter()
            .map(|(dependency_path, package_snapshot)| async move {
                InstallPackageBySnapshot {
                    tarball_cache,
                    http_client,
                    config,
                    dependency_path,
                    package_snapshot,
                }
                .install()
                .await
                .unwrap(); // TODO: properly propagate this error
            })
            .pipe(future::join_all)
            .await;
    }
}
