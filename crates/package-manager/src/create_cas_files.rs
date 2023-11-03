use crate::{link_file, LinkFileError};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{ffi::OsStr, path::Path};

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// If `dir_path` doesn't exist, create and populate it with files from `cas_paths`.
///
/// If `dir_path` already exists, do nothing.
pub fn create_cas_files<'cas_paths, CasPathList: ?Sized, CasPathKey, CasPathValue>(
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: &'cas_paths CasPathList,
) -> Result<(), CreateCasFilesError>
where
    CasPathList: IntoParallelRefIterator<'cas_paths, Item = (CasPathKey, CasPathValue)>,
    CasPathKey: AsRef<OsStr> + 'cas_paths,
    CasPathValue: AsRef<Path> + 'cas_paths,
{
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
            link_file(store_path.as_ref(), &dir_path.join(cleaned_entry.as_ref()))
        })
        .map_err(CreateCasFilesError::LinkFile)
}
