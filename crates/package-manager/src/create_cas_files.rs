use crate::{link_file, LinkFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{IoSendValue, IoThread};
use pacquet_npmrc::PackageImportMethod;
use pipe_trait::Pipe;
use std::{
    collections::HashMap,
    ffi::OsString,
    iter,
    path::{Path, PathBuf},
};

/// Value type of [`create_cas_files`].
type CreateCasFilesValue = Box<dyn Iterator<Item = IoSendValue>>;

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
    io_thread: &IoThread,
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &HashMap<OsString, PathBuf>,
) -> Result<CreateCasFilesValue, CreateCasFilesError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only PackageImportMethod::Auto is currently supported, but {dir_path:?} requires {import_method:?}",
    );

    if dir_path.exists() {
        return iter::empty().pipe(|x| Box::new(x) as CreateCasFilesValue).pipe(Ok);
    }

    cas_paths
        .iter()
        .map(|(cleaned_entry, store_path)| {
            link_file(io_thread, store_path, &dir_path.join(cleaned_entry))
        })
        .collect::<Result<Vec<_>, LinkFileError>>()
        .map_err(CreateCasFilesError::LinkFile)?
        .into_iter()
        .flatten()
        .pipe(|x| Box::new(x) as CreateCasFilesValue)
        .pipe(Ok)
}
