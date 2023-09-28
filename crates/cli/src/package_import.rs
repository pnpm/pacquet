use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::package_manager::{AutoImportError, PackageManagerError};
use pacquet_diagnostics::tracing;
use pacquet_lockfile::{DependencyPath, PackageSnapshot, PkgNameVerPeer};
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;

pub trait ImportMethodImpl {
    fn import(
        &self,
        cas_files: &HashMap<OsString, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError>;
}

impl ImportMethodImpl for PackageImportMethod {
    fn import(
        &self,
        cas_files: &HashMap<OsString, PathBuf>,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), PackageManagerError> {
        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_to, "Import package");
        match self {
            PackageImportMethod::Auto => {
                if !save_path.exists() {
                    cas_files
                        .into_par_iter()
                        .try_for_each(|(cleaned_entry, store_path)| {
                            auto_import(store_path, &save_path.join(cleaned_entry))
                        })
                        .expect("expected no write errors");
                }

                if !symlink_to.is_symlink() {
                    if let Some(parent_dir) = symlink_to.parent() {
                        fs::create_dir_all(parent_dir)?;
                    }
                    crate::fs::symlink_dir(save_path, symlink_to)?;
                }
            }
            _ => panic!("Not implemented yet"),
        }

        Ok(())
    }
}

/// This function does 2 things:
/// 1. Install the files from `cas_paths`
/// 2. Create the symlink layout
///
/// **TODO:** may break this function into 2 later
pub fn install_virtdir_by_snapshot(
    dependency_path: &DependencyPath,
    virtual_store_dir: &Path,
    cas_paths: &HashMap<OsString, PathBuf>,
    import_method: PackageImportMethod,
    package_snapshot: &PackageSnapshot,
) -> Result<(), PackageManagerError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only auto import method is supported, but {dependency_path} requires {import_method:?}",
    );

    // node_modules/.pacquet/pkg-name@x.y.z/node_modules
    let virtual_node_modules_dir =
        virtual_store_dir.join(dependency_path.to_virtual_store_name()).join("node_modules");

    // 1. Install the files from `cas_paths`
    let save_path = virtual_node_modules_dir.join(&dependency_path.package_specifier.name);
    if !save_path.exists() {
        cas_paths.into_par_iter().try_for_each(|(cleaned_entry, store_path)| {
            auto_import(store_path, &save_path.join(cleaned_entry))
        })?;
    }

    // 2. Create the symlink layout
    if let Some(dependencies) = &package_snapshot.dependencies {
        dependencies.par_iter().for_each(|(name, ver_peer)| {
            let custom_registry = None; // assuming all registries are default registries (custom registry is not yet supported)
            let package_specifier = PkgNameVerPeer::new(name.to_string(), ver_peer.clone()); // TODO: remove copying here
            let dependency_path = DependencyPath { custom_registry, package_specifier };
            let virtual_store_name = dependency_path.to_virtual_store_name();
            // NOTE: symlink target in pacquet is absolute yet in pnpm is relative
            // TODO: change symlink target to relative
            let symlink_target =
                virtual_store_dir.join(virtual_store_name).join("node_modules").join(name);
            let symlink_path = virtual_node_modules_dir.join(name);
            if let Err(error) = crate::fs::symlink_dir(&symlink_target, &symlink_path) {
                match error.kind() {
                    ErrorKind::AlreadyExists => {},
                    _ => panic!("Failed to create symlink at {symlink_path:?} to {symlink_target:?}: {error}"), // TODO: proper error propagation
                }
            }
        });
    }

    Ok(())
}

fn auto_import(source_file: &Path, target_link: &Path) -> Result<(), AutoImportError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| AutoImportError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
        AutoImportError::CreateLink {
            from: source_file.to_path_buf(),
            to: target_link.to_path_buf(),
            error,
        }
    })?; // TODO: add hardlink

    Ok(())
}
