use crate::{create_cas_files, create_symlink_layout, CreateCasFilesError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::{DependencyPath, PackageSnapshot};
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
}

/// This function does 2 things:
/// 1. Install the files from `cas_paths`
/// 2. Create the symlink layout
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
    create_cas_files(&save_path, cas_paths).map_err(CreateVirtualDirError::CreateCasFiles)?;

    // 2. Create the symlink layout
    if let Some(dependencies) = &package_snapshot.dependencies {
        create_symlink_layout(dependencies, virtual_store_dir, &virtual_node_modules_dir)
    }

    Ok(())
}
