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
/// The virtual-store package directory
/// (`node_modules/.pacquet/{name}@{version}/node_modules/`) is expected to
/// already exist — it's created up-front in
/// [`CreateVirtualStore::run`](crate::CreateVirtualStore::run) so intra-package
/// symlinking can run concurrently with tarball fetching. This subroutine only
/// populates the package's own files.
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
