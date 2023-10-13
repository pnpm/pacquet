use crate::{InstallFrozenLockfile, InstallWithoutLockfile};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_tarball::Cache;
use reqwest::Client;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Configuration read from `.npmrc`.
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub package_json: &'a PackageJson,
    /// Data from the `pnpm-lock.yaml` file.
    pub lockfile: Option<&'a Lockfile>,
    /// List of [`DependencyGroup`]s.
    pub dependency_groups: DependencyGroupList,
    /// Whether `--frozen-lockfile` is specified.
    pub frozen_lockfile: bool,
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) {
        let Install {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile,
            dependency_groups,
            frozen_lockfile,
        } = self;

        tracing::info!(target: "pacquet::install", "Start all");

        match (config.lockfile, frozen_lockfile, lockfile) {
            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_cache,
                    http_client,
                    config,
                    package_json,
                    dependency_groups,
                }
                .run()
                .await;
            }
            (true, false, Some(_)) | (true, false, None) | (true, true, None) => {
                unimplemented!();
            }
            (true, true, Some(lockfile)) => {
                let Lockfile { lockfile_version, project_snapshot, packages, .. } = lockfile;
                assert_eq!(lockfile_version.major, 6); // compatibility check already happens at serde, but this still helps preventing programmer mistakes.

                InstallFrozenLockfile {
                    tarball_cache,
                    http_client,
                    config,
                    project_snapshot,
                    packages: packages.as_ref(),
                    dependency_groups,
                }
                .run()
                .await;
            }
        }

        tracing::info!(target: "pacquet::install", "Complete all");
    }
}
