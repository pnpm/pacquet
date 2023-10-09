use crate::{create_cas_files, symlink_pkg, CreateCasFilesError, SymlinkPackageError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Error type for [`ImportPackage`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum ImportPackageError {
    CreateCasFiles(CreateCasFilesError),
    SymlinkPackage(SymlinkPackageError),
}

/// This subroutine does 2 things:
/// 1. Populate files into [`save_path`](Self::save_path) according to [`cas_paths`](Self::cas_paths) and [`method`](Self::method).
/// 2. Create at [`symlink_path`](Self::symlink_path) which points to [`save_path`](Self::save_path).
#[must_use]
pub struct ImportPackage<'a> {
    pub method: PackageImportMethod,
    pub cas_paths: &'a HashMap<OsString, PathBuf>,
    pub save_path: &'a Path,
    pub symlink_path: &'a Path,
}

impl<'a> ImportPackage<'a> {
    pub fn import_pkg(self) -> Result<(), ImportPackageError> {
        let ImportPackage { method, cas_paths, save_path, symlink_path } = self;

        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_path, "Import package");
        match method {
            PackageImportMethod::Auto => {
                create_cas_files(save_path, cas_paths)
                    .map_err(ImportPackageError::CreateCasFiles)?;
            }
            _ => panic!("Not implemented yet"),
        }

        symlink_pkg(save_path, symlink_path).map_err(ImportPackageError::SymlinkPackage)
    }
}
