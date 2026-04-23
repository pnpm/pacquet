use crate::{InstallPackageBySnapshot, InstallPackageBySnapshotError};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, RootProjectSnapshot};
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pipe_trait::Pipe;
use std::collections::HashMap;

/// This subroutine generates filesystem layout for the virtual store at `node_modules/.pacquet`.
#[must_use]
pub struct CreateVirtualStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub packages: Option<&'a HashMap<DependencyPath, PackageSnapshot>>,
    pub project_snapshot: &'a RootProjectSnapshot,
}

/// Error type of [`CreateVirtualStore`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualStoreError {
    #[diagnostic(transparent)]
    InstallPackageBySnapshot(#[error(source)] InstallPackageBySnapshotError),
}

impl<'a> CreateVirtualStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<(), CreateVirtualStoreError> {
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
                    .map_err(CreateVirtualStoreError::InstallPackageBySnapshot)
            })
            .pipe(future::try_join_all)
            .await?;

        Ok(())
    }
}
