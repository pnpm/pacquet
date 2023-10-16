use crate::Install;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::PackageJsonError;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_registry::{PackageTag, PackageVersion};
use pacquet_tarball::Cache;
use reqwest::Client;

/// This subroutine does everything `pacquet add` is supposed to do.
#[must_use]
pub struct Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Configuration read from `.npmrc`.
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub package_json: &'a mut PackageJson,
    /// Data from the `pnpm-lock.yaml` file.
    pub lockfile: Option<&'a Lockfile>,
    /// Function that creates an iterator [`DependencyGroup`]s.
    pub list_dependency_groups: ListDependencyGroups, // must be a function because it is called multiple times
    /// Name of the package to add.
    pub package: &'a str,
    /// Whether `--save-exact` is provided.
    pub save_exact: bool, // TODO: add `save-exact` to `.npmrc`, merge configs, and remove this
}

/// Error type of [`Add`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum AddError {
    #[display("Failed to add package to manifest: {_0}")]
    AddDependencyToPackageJson(#[error(source)] PackageJsonError),
    #[display("Failed save the manifest file: {_0}")]
    SavePackageJson(#[error(source)] PackageJsonError),
}

impl<'a, ListDependencyGroups, DependencyGroupList>
    Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub async fn run(self) -> Result<(), AddError> {
        let Add {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile,
            list_dependency_groups,
            package,
            save_exact,
        } = self;

        let latest_version = PackageVersion::fetch_from_registry(
            package,
            PackageTag::Latest, // TODO: add support for specifying tags
            http_client,
            &config.registry,
        )
        .await
        .expect("resolve latest tag"); // TODO: properly propagate this error

        let version_range = latest_version.serialize(save_exact);
        for dependency_group in list_dependency_groups() {
            package_json
                .add_dependency(package, &version_range, dependency_group)
                .map_err(AddError::AddDependencyToPackageJson)?;
        }

        Install {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile,
            dependency_groups: list_dependency_groups(),
            frozen_lockfile: false,
        }
        .run()
        .await;

        package_json.save().map_err(AddError::SavePackageJson)?;

        Ok(())
    }
}
