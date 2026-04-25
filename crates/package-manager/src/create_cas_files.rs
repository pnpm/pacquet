use crate::{EnsuredDirsCache, LinkFileError, link_file};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::HashMap,
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
    cas_paths: &HashMap<String, PathBuf>,
) -> Result<(), CreateCasFilesError> {
    if dir_path.exists() {
        return Ok(());
    }

    // Per-package parent-dir cache: every file in the same package
    // hangs off `dir_path`, and most of them share intermediate dirs
    // (`lib/`, `dist/`, `src/index.js`'s `src/`, …). Allocating once
    // per `create_cas_files` call lets rayon workers share dedup
    // state without ballooning into a process-wide cache that would
    // need install-scope teardown to stay correct under bench-style
    // `rm -rf node_modules`.
    let ensured_dirs_cache = EnsuredDirsCache::new();

    cas_paths
        .par_iter()
        .try_for_each(|(cleaned_entry, store_path)| {
            link_file(import_method, store_path, &dir_path.join(cleaned_entry), &ensured_dirs_cache)
        })
        .map_err(CreateCasFilesError::LinkFile)
}
