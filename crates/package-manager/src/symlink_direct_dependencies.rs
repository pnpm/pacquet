use crate::{link_direct_dep_bins, symlink_package};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::LinkBinsError;
use pacquet_lockfile::{Lockfile, PkgName, PkgNameVerPeer, PkgVerPeer, ProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use std::{collections::HashMap, path::Path};

/// This subroutine creates symbolic links in the `node_modules` directory for
/// the direct dependencies. The targets of the link are the virtual directories.
///
/// If package `foo@x.y.z` is declared as a dependency in `package.json`,
/// symlink `foo -> .pacquet/foo@x.y.z/node_modules/foo` shall be created
/// in the `node_modules` directory.
#[must_use]
pub struct SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub config: &'static Npmrc,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub dependency_groups: DependencyGroupList,
}

/// Error type of [`SymlinkDirectDependencies`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum SymlinkDirectDependenciesError {
    #[display(
        "Lockfile has no `importers.{root_key:?}` entry for the root project; pacquet cannot decide which direct dependencies to symlink into `node_modules`."
    )]
    #[diagnostic(code(pacquet_package_manager::missing_root_importer))]
    MissingRootImporter { root_key: String },

    #[diagnostic(transparent)]
    LinkBins(#[error(source)] LinkBinsError),
}

impl<'a, DependencyGroupList> SymlinkDirectDependencies<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), SymlinkDirectDependenciesError> {
        let SymlinkDirectDependencies { config, importers, dependency_groups } = self;

        let project_snapshot = importers.get(Lockfile::ROOT_IMPORTER_KEY).ok_or_else(|| {
            SymlinkDirectDependenciesError::MissingRootImporter {
                root_key: Lockfile::ROOT_IMPORTER_KEY.to_string(),
            }
        })?;

        let direct_deps: Vec<(PkgName, PkgVerPeer)> = project_snapshot
            .dependencies_by_groups(dependency_groups)
            .map(|(name, spec)| (PkgName::clone(name), spec.version.clone()))
            .collect();

        symlink_direct_deps_into_node_modules(
            &config.modules_dir,
            &config.virtual_store_dir,
            &direct_deps,
        );

        let dep_names: Vec<String> = direct_deps.iter().map(|(name, _)| name.to_string()).collect();
        link_direct_dep_bins(&config.modules_dir, &dep_names)
            .map_err(SymlinkDirectDependenciesError::LinkBins)?;

        Ok(())
    }
}

/// Create the per-direct-dependency `<modules_dir>/<name> ->
/// <virtual_store_dir>/<name>@<version>/node_modules/<name>` symlinks.
///
/// Pure filesystem operation extracted from
/// [`SymlinkDirectDependencies::run`] so it is unit-testable with a real
/// `tempdir` instead of needing a full lockfile, npmrc, and
/// project-snapshot scaffold. The caller has already filtered the
/// dependency list (e.g. applied `dependency_groups`); this function
/// just executes the link step.
///
/// Driven on rayon because each link is independent. Mirrors the same
/// shape as [`crate::link_direct_dep_bins`].
pub fn symlink_direct_deps_into_node_modules(
    modules_dir: &Path,
    virtual_store_dir: &Path,
    deps: &[(PkgName, PkgVerPeer)],
) {
    use rayon::prelude::*;
    deps.par_iter().for_each(|(name, version)| {
        let virtual_store_name =
            PkgNameVerPeer::new(PkgName::clone(name), version.clone()).to_virtual_store_name();

        let name_str = name.to_string();
        symlink_package(
            &virtual_store_dir.join(virtual_store_name).join("node_modules").join(&name_str),
            &modules_dir.join(&name_str),
        )
        .expect("symlink pkg"); // TODO: properly propagate this error
    });
}

#[cfg(test)]
mod tests;
