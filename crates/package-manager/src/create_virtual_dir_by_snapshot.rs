use crate::{create_cas_files, CreateCasFilesError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_lockfile::PackageKey;
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

/// Install extracted CAS files into the package's slot in the virtual store.
///
/// Runs concurrently with the symlink branch in
/// [`CreateVirtualStore::run`](crate::CreateVirtualStore::run), so the virtual-
/// store package directory (`node_modules/.pacquet/{name}@{version}/node_modules/`)
/// may or may not exist when this subroutine is called — either the symlink
/// branch has already `create_dir_all`'d it, or it doesn't yet. Populating the
/// package's own files is safe in both cases: `link_file` calls
/// `fs::create_dir_all` on every target's parent before importing, so any
/// missing ancestor is materialised on demand.
#[must_use]
pub struct CreateVirtualDirBySnapshot<'a> {
    pub virtual_store_dir: &'a Path,
    pub cas_paths: &'a HashMap<String, PathBuf>,
    pub import_method: PackageImportMethod,
    pub package_key: &'a PackageKey,
}

/// Error type of [`CreateVirtualDirBySnapshot`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateVirtualDirError {
    #[diagnostic(transparent)]
    CreateCasFiles(#[error(source)] CreateCasFilesError),
}

impl<'a> CreateVirtualDirBySnapshot<'a> {
    /// Execute the subroutine.
    pub fn run(self) -> Result<(), CreateVirtualDirError> {
        let CreateVirtualDirBySnapshot { virtual_store_dir, cas_paths, import_method, package_key } =
            self;

        let save_path = virtual_store_dir
            .join(package_key.to_virtual_store_name())
            .join("node_modules")
            .join(package_key.name.to_string());
        create_cas_files(import_method, &save_path, cas_paths)
            .map_err(CreateVirtualDirError::CreateCasFiles)
    }
}
