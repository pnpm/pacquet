use crate::{create_cas_files, create_symlink_layout, CreateCasFilesError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

/// This subroutine installs the files from [`cas_paths`](Self::cas_paths) then creates the symlink layout.
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    /// Path to the virtual store dir (usually canonical paths of `node_modules/.pacquet`).
    pub virtual_store_dir: &'a Path,
    /// CAS files map.
    pub cas_paths: &'a HashMap<OsString, PathBuf>,
    /// Import method.
    pub import_method: PackageImportMethod,
    /// Key of the package map from the lockfile.
    pub dependency_path: &'a DependencyPath,
    /// Value of the package map from the lockfile.
    pub package_snapshot: &'a PackageSnapshot,
}

/// Error type of [`CreateVirtualDirBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[display(fmt = "Failed to recursively create node_modules directory at {dir:?}: {error}")]
    #[diagnostic(code(pacquet_package_manager::create_node_modules_dir))]
    CreateNodeModulesDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot {
            virtual_store_dir,
            cas_paths,
            import_method,
            dependency_path,
            package_snapshot,
        } = self;

        // node_modules/.pacquet/pkg-name@x.y.z/node_modules
        let virtual_node_modules_dir = virtual_store_dir
            .join(dependency_path.package_specifier.to_virtual_store_name())
            .join("node_modules");
        fs::create_dir_all(&virtual_node_modules_dir).map_err(|error| {
            CreateVirtualDirError::CreateNodeModulesDir {
                dir: virtual_node_modules_dir.to_path_buf(),
                error,
            }
        })?;

        // 1. Install the files from `cas_paths`
        let save_path =
            virtual_node_modules_dir.join(dependency_path.package_specifier.name.to_string());
        create_cas_files(import_method, &save_path, cas_paths)
            .map_err(CreateVirtualDirError::CreateCasFiles)?;

        // 2. Create the symlink layout
        if let Some(dependencies) = &package_snapshot.dependencies {
            create_symlink_layout(dependencies, virtual_store_dir, &virtual_node_modules_dir)
        }

        Ok(())
    }
}
