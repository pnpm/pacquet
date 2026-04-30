use crate::{Install, InstallError, ResolvedPackages};
use derive_more::{Display, Error};
use miette::Diagnostic;
use node_semver::{Range, Version};
use pacquet_lockfile::Lockfile;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifestError;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_registry::{PackageTag, PackageVersion, RegistryError};
use pacquet_tarball::MemCache;

/// This subroutine does everything `pacquet add` is supposed to do.
#[must_use]
pub struct Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub resolved_packages: &'a ResolvedPackages,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub manifest: &'a mut PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub list_dependency_groups: ListDependencyGroups, // must be a function because it is called multiple times
    pub packages: &'a [&'a str],
    pub save_exact: bool, // TODO: add `save-exact` to `.npmrc`, merge configs, and remove this
}

/// Error type of [`Add`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum AddError {
    #[display("Failed to fetch version for package: {_0}")]
    FetchVersion(#[error(source)] RegistryError),
    #[display("Failed to add package to manifest: {_0}")]
    AddDependencyToManifest(#[error(source)] PackageManifestError),
    #[display("Failed save the manifest file: {_0}")]
    SaveManifest(#[error(source)] PackageManifestError),
    #[diagnostic(transparent)]
    Install(#[error(source)] InstallError),
}

/// Split a package argument into name and version specifier.
///
/// Handles scoped packages: `@scope/name@1.0.0` splits into `("@scope/name", "1.0.0")`.
fn parse_pkg_arg(arg: &str) -> (&str, &str) {
    let start = usize::from(arg.starts_with('@'));
    match arg[start..].find('@') {
        Some(pos) => {
            let split = start + pos;
            (&arg[..split], &arg[split + 1..])
        }
        None => (arg, ""),
    }
}

impl<'a, ListDependencyGroups, DependencyGroupList>
    Add<'a, ListDependencyGroups, DependencyGroupList>
where
    ListDependencyGroups: Fn() -> DependencyGroupList,
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub async fn run(self) -> Result<(), AddError> {
        let Add {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            list_dependency_groups,
            packages,
            save_exact,
            resolved_packages,
        } = self;

        for &pkg in packages {
            let (name, specifier) = parse_pkg_arg(pkg);

            // Resolve the version specifier to a range string to save in package.json.
            // For tags (no specifier or dist-tag), we fetch the resolved version first so
            // we can save a pinned semver range rather than a mutable tag name.
            let version_to_save = if specifier.is_empty() || specifier == "latest" {
                let version = PackageVersion::fetch_from_registry(
                    name,
                    PackageTag::Latest,
                    http_client,
                    &config.registry,
                )
                .await
                .map_err(AddError::FetchVersion)?;
                version.serialize(save_exact)
            } else if let Ok(v) = specifier.parse::<Version>() {
                // Exact semver version: fetch to validate, then save with ^ unless --save-exact.
                PackageVersion::fetch_from_registry(
                    name,
                    PackageTag::Version(v),
                    http_client,
                    &config.registry,
                )
                .await
                .map_err(AddError::FetchVersion)?;
                if save_exact { specifier.to_owned() } else { format!("^{specifier}") }
            } else if specifier.parse::<Range>().is_ok() {
                // Semver range (e.g. `^18`, `~1.0.0`, `>=1 <2`): save as-is and let
                // the install step resolve the best matching version.
                specifier.to_owned()
            } else {
                // Named dist-tag (e.g. `next`, `beta`): resolve to a concrete version.
                let version = PackageVersion::fetch_from_registry(
                    name,
                    PackageTag::Tag(specifier.to_owned()),
                    http_client,
                    &config.registry,
                )
                .await
                .map_err(AddError::FetchVersion)?;
                version.serialize(save_exact)
            };

            for dependency_group in list_dependency_groups() {
                manifest
                    .add_dependency(name, &version_to_save, dependency_group)
                    .map_err(AddError::AddDependencyToManifest)?;
            }
        }

        Install {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            dependency_groups: list_dependency_groups(),
            frozen_lockfile: false,
            resolved_packages,
        }
        .run()
        .await
        .map_err(AddError::Install)?;

        manifest.save().map_err(AddError::SaveManifest)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pkg_arg_no_specifier() {
        assert_eq!(parse_pkg_arg("react"), ("react", ""));
    }

    #[test]
    fn parse_pkg_arg_with_version() {
        assert_eq!(parse_pkg_arg("react@18.2.0"), ("react", "18.2.0"));
    }

    #[test]
    fn parse_pkg_arg_with_range() {
        assert_eq!(parse_pkg_arg("react@^18"), ("react", "^18"));
    }

    #[test]
    fn parse_pkg_arg_with_tag() {
        assert_eq!(parse_pkg_arg("react@next"), ("react", "next"));
    }

    #[test]
    fn parse_pkg_arg_scoped_no_specifier() {
        assert_eq!(parse_pkg_arg("@scope/pkg"), ("@scope/pkg", ""));
    }

    #[test]
    fn parse_pkg_arg_scoped_with_version() {
        assert_eq!(parse_pkg_arg("@scope/pkg@1.0.0"), ("@scope/pkg", "1.0.0"));
    }

    #[test]
    fn parse_pkg_arg_scoped_with_range() {
        assert_eq!(parse_pkg_arg("@scope/pkg@^1.0.0"), ("@scope/pkg", "^1.0.0"));
    }
}
