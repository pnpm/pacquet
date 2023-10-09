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

/// This subroutine does 2 things:
/// 1. Install the files from `cas_paths`
/// 2. Create the symlink layout
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    pub dependency_path: &'a DependencyPath,
    pub virtual_store_dir: &'a Path,
    pub cas_paths: &'a HashMap<OsString, PathBuf>,
    pub import_method: PackageImportMethod,
    pub package_snapshot: &'a PackageSnapshot,
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn create_virtual_dir_by_snapshot(self) -> Result<(), CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot {
            dependency_path,
            virtual_store_dir,
            cas_paths,
            import_method,
            package_snapshot,
        } = self;

        assert_eq!(
            import_method,
            PackageImportMethod::Auto,
            "Only auto import method is supported, but {dependency_path} requires {import_method:?}",
        );

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
        create_cas_files(&save_path, cas_paths).map_err(CreateVirtualDirError::CreateCasFiles)?;

        // 2. Create the symlink layout
        if let Some(dependencies) = &package_snapshot.dependencies {
            create_symlink_layout(dependencies, virtual_store_dir, &virtual_node_modules_dir)
        }

        Ok(())
    }
}