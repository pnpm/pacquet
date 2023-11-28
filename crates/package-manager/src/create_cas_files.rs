use crate::{link_file, LinkFileError};
use derive_more::{Display, Error};
use futures_util::future;
use miette::Diagnostic;
use pacquet_fs::EnsureFileError;
use pacquet_npmrc::PackageImportMethod;
use pacquet_tarball::CasMap;
use std::path::Path;

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[diagnostic(transparent)]
    WriteStoreFile(#[error(source)] EnsureFileError),
    #[diagnostic(transparent)]
    LinkFile(#[error(source)] LinkFileError),
}

/// If `dir_path` doesn't exist, create and populate it with files from `cas_paths`.
///
/// If `dir_path` already exists, do nothing.
pub async fn create_cas_files(
    import_method: PackageImportMethod,
    dir_path: &Path,
    cas_paths: CasMap,
) -> Result<(), CreateCasFilesError> {
    assert_eq!(
        import_method,
        PackageImportMethod::Auto,
        "Only PackageImportMethod::Auto is currently supported, but {dir_path:?} requires {import_method:?}",
    );

    if dir_path.exists() {
        return Ok(());
    }

    let futures = cas_paths.into_iter().map(|(cleaned_entry, store_path_join_handle)| async {
        let store_path = store_path_join_handle.await.expect("no join error")?;
        let target_path = dir_path.join(cleaned_entry);
        let link_file_handle =
            tokio::task::spawn_blocking(move || link_file(&store_path, &target_path));
        Ok::<_, EnsureFileError>(link_file_handle)
    });

    for handle in future::join_all(futures).await {
        handle
            .map_err(CreateCasFilesError::WriteStoreFile)?
            .await
            .expect("no join error")
            .map_err(CreateCasFilesError::LinkFile)?;
    }

    Ok(())
}
