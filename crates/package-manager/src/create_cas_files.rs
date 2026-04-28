use crate::{LinkFileError, link_file};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

/// Error type for [`create_cas_files`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum CreateCasFilesError {
    #[display("cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
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

    // Mirror pnpm v11's `tryImportIndexedDir` (see
    // `fs/indexed-pkg-importer/src/importIndexedDir.ts`): collect the
    // unique relative parent dirs from the file map, sort them
    // shortest-first, then mkdir each one sequentially before any
    // file imports start. Sorting by length means the recursive
    // mkdir for a deeper dir always finds its ancestor already on
    // disk, so each call costs one `mkdirat` instead of walking up.
    // After the pre-pass `link_file` itself has no mkdir cost — the
    // hot rayon loop only does the actual hardlink / clone / copy.
    let mut rel_dirs: HashSet<&str> = HashSet::new();
    for entry in cas_paths.keys() {
        if let Some(parent) = Path::new(entry).parent()
            && let Some(rel) = parent.to_str()
            && !rel.is_empty()
        {
            rel_dirs.insert(rel);
        }
    }

    // The package root itself: pnpm's `importIndexedDir` mkdirs
    // `newDir` before calling `tryImportIndexedDir`, so do that here
    // too. Files at the package root (e.g. `package.json`) need this
    // even when `rel_dirs` is empty.
    fs::create_dir_all(dir_path).map_err(|error| CreateCasFilesError::CreateDir {
        dirname: dir_path.to_path_buf(),
        error,
    })?;

    let mut ordered: Vec<&str> = rel_dirs.into_iter().collect();
    ordered.sort_by_key(|s| s.len());
    for rel in ordered {
        let abs = dir_path.join(rel);
        fs::create_dir_all(&abs)
            .map_err(|error| CreateCasFilesError::CreateDir { dirname: abs, error })?;
    }

    cas_paths
        .par_iter()
        .try_for_each(|(cleaned_entry, store_path)| {
            link_file(import_method, store_path, &dir_path.join(cleaned_entry))
        })
        .map_err(CreateCasFilesError::LinkFile)
}
