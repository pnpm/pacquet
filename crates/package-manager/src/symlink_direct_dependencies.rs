use crate::symlink_package;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_cmd_shim::{LinkBinsError, PackageBinSource, link_bins_of_packages};
use pacquet_lockfile::{Lockfile, PkgName, PkgNameVerPeer, PkgVerPeer, ProjectSnapshot};
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::DependencyGroup;
use rayon::prelude::*;
use std::{collections::HashMap, fs, path::PathBuf};

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

        direct_deps.par_iter().for_each(|(name, version)| {
            // TODO: the code below is not optimal
            let virtual_store_name =
                PkgNameVerPeer::new(PkgName::clone(name), version.clone()).to_virtual_store_name();

            let name_str = name.to_string();
            symlink_package(
                &config
                    .virtual_store_dir
                    .join(virtual_store_name)
                    .join("node_modules")
                    .join(&name_str),
                &config.modules_dir.join(&name_str),
            )
            .expect("symlink pkg"); // TODO: properly propagate this error
        });

        // After the symlink layout is in place, link each direct
        // dependency's bins into `<modules_dir>/.bin`. Mirrors pnpm v11's
        // `linkBinsOfPackages` call site at
        // <https://github.com/pnpm/pnpm/blob/4750fd370c/installing/deps-installer/src/install/index.ts#L1539>.
        let direct_dep_locations: Vec<PathBuf> =
            direct_deps.iter().map(|(name, _)| config.modules_dir.join(name.to_string())).collect();
        let bin_sources = collect_bin_sources(&direct_dep_locations);
        if !bin_sources.is_empty() {
            link_bins_of_packages(&bin_sources, &config.modules_dir.join(".bin"))
                .map_err(SymlinkDirectDependenciesError::LinkBins)?;
        }

        Ok(())
    }
}

/// Read the `package.json` for each location in `locations` and return the
/// ones that parse. Locations are direct-dependency symlinks under
/// `<modules_dir>/<name>` — pacquet has already created them by this point,
/// and `fs::read` follows the symlink to the real file in the virtual store.
fn collect_bin_sources(locations: &[PathBuf]) -> Vec<PackageBinSource> {
    locations
        .iter()
        .filter_map(|location| {
            let manifest_path = location.join("package.json");
            let bytes = fs::read(&manifest_path).ok()?;
            let manifest = serde_json::from_slice(&bytes).ok()?;
            Some(PackageBinSource { location: location.clone(), manifest })
        })
        .collect()
}
