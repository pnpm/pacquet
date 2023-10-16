use crate::{link_file, LinkFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// If `dir_path` doesn't exist, create and populate it with files from `cas_paths`.
///
/// If `dir_path` already exists, do nothing.
pub fn create_cas_files(
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<OsString, PathBuf>,
) -> Result<(), CreateCasFilesError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only PackageImportMethod::Auto is currently supported, but {dir_path:?} requires {import_method:?}",
    );

    if dir_path.exists() {
        return Ok(());
    }

    cas_paths
        .par_iter()
        .try_for_each(|(cleaned_entry, store_path)| {
            link_file(store_path, &dir_path.join(cleaned_entry))
        })
        .map_err(CreateCasFilesError::LinkFile)
}
