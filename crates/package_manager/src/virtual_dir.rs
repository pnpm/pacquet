use crate::{link_file, symlink_pkg, LinkFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{
    DependencyPath, PackageSnapshot, PackageSnapshotDependency, PkgNameVerPeer,
};
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// This function does 2 things:
/// 1. Install the files from `cas_paths`
/// 2. Create the symlink layout
///
/// **TODO:** may break this function into 2 later
pub fn create_virtual_dir_by_snapshot(
    dependency_path: &DependencyPath,
    virtual_store_dir: &Path,
    cas_paths: &HashMap<OsString, PathBuf>,
    import_method: PackageImportMethod,
    package_snapshot: &PackageSnapshot,
) -> Result<(), CreateVirtualDirError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only auto import method is supported, but {dependency_path} requires {import_method:?}",
    );

    // node_modules/.pacquet/pkg-name@x.y.z/node_modules
    let virtual_node_modules_dir = virtual_store_dir
        .join(dependency_path.package_specifier.to_virtual_store_name())
        .join("node_modules");
    fs::create_dir_all(&virtual_node_modules_dir).unwrap_or_else(|error| {
        panic!("Failed to create directory at {virtual_node_modules_dir:?}: {error}")
    }); // TODO: proper error propagation

    // 1. Install the files from `cas_paths`
    let save_path =
        virtual_node_modules_dir.join(dependency_path.package_specifier.name.to_string());
    if !save_path.exists() {
        cas_paths.par_iter().try_for_each(|(cleaned_entry, store_path)| {
            link_file(store_path, &save_path.join(cleaned_entry))
                .map_err(CreateVirtualDirError::LinkFile)
        })?;
    }

    // 2. Create the symlink layout
    if let Some(dependencies) = &package_snapshot.dependencies {
        dependencies.par_iter().for_each(|(name, spec)| {
            let virtual_store_name = match spec {
                PackageSnapshotDependency::PkgVerPeer(ver_peer) => {
                    let package_specifier = PkgNameVerPeer::new(name.clone(), ver_peer.clone()); // TODO: remove copying here
                    package_specifier.to_virtual_store_name()
                }
                PackageSnapshotDependency::DependencyPath(dependency_path) => {
                    dependency_path.package_specifier.to_virtual_store_name()
                }
            };
            let name_str = name.to_string();
            symlink_pkg(
                &virtual_store_dir.join(virtual_store_name).join("node_modules").join(&name_str),
                &virtual_node_modules_dir.join(&name_str),
            );
        });
    }

    Ok(())
}
