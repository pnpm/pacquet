//! CAS I/O helpers shared between [`crate::GitFetcher`] (git clone +
//! `preparePackage`) and [`crate::GitHostedTarballFetcher`] (tarball
//! download + `preparePackage`).
//!
//! - [`materialize_into`] copies CAS-resident files into a fresh
//!   working directory so the prepare phase has a writable tree.
//!   Used by the git-hosted tarball fetcher: by the time the tarball
//!   has been downloaded by `pacquet-tarball`, the files already live
//!   in the CAS, so the prepare phase reads them out into a temp dir
//!   it can mutate without corrupting the CAS.
//! - [`import_into_cas`] writes a prepared file set back to the CAS
//!   and produces the `relative-path → cas-path` map the install
//!   dispatcher hands to `CreateVirtualDirBySnapshot`.
//! - [`is_file_executable`] / [`map_write_cas`] are minor helpers
//!   factored out alongside the import.

use crate::error::GitFetcherError;
use pacquet_store_dir::StoreDir;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// Copy every CAS file referenced in `cas_paths` into `target_dir`,
/// preserving relative paths. CAS files are hardlinked-or-copied per
/// install elsewhere, but for the prepare phase the working tree must
/// be writable *without* mutating the shared CAS entry, so this path
/// always allocates fresh inodes via [`fs::copy`].
///
/// Mirrors the effect of upstream's
/// [`cafs.importPackage(tempLocation, …)`](https://github.com/pnpm/pnpm/blob/94240bc046/fetching/tarball-fetcher/src/gitHostedTarballFetcher.ts#L75)
/// call inside `prepareGitHostedPkg`, but produces a *standalone*
/// directory rather than a pnpm-style CAFS slot — pacquet's `StoreDir`
/// only knows how to import on the way *in*, and the prepare phase
/// needs raw filesystem semantics for scripts to run.
pub(crate) fn materialize_into(
    cas_paths: &HashMap<String, PathBuf>,
    target_dir: &Path,
) -> Result<(), GitFetcherError> {
    for (rel, cas_path) in cas_paths {
        let target = target_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(GitFetcherError::Io)?;
        }
        fs::copy(cas_path, &target).map_err(GitFetcherError::Io)?;
        // Carry the executable bit across. The CAS uses a `-exec`
        // suffix on the file name to encode the bit (matches pnpm's
        // CAFS layout), so reading it back from the path is the only
        // reliable signal — `fs::copy` itself doesn't reset POSIX
        // permissions, but we may need to *add* the bit if the CAS
        // file's filesystem-level mode lost it during an earlier
        // copy or hardlink path elsewhere.
        #[cfg(unix)]
        if cas_path_is_executable(cas_path) {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&target).map_err(GitFetcherError::Io)?.permissions();
            perms.set_mode(perms.mode() | 0o111);
            fs::set_permissions(&target, perms).map_err(GitFetcherError::Io)?;
        }
    }
    Ok(())
}

/// Write each file in `files` (relative to `pkg_dir`) into the CAS,
/// returning the map the caller hands to `CreateVirtualDirBySnapshot`.
/// Mirrors the role of upstream's
/// [`addFilesFromDir`](https://github.com/pnpm/pnpm/blob/94240bc046/store/cafs/src/addFilesFromDir.ts)
/// on the post-prepare write side.
pub(crate) fn import_into_cas(
    store_dir: &StoreDir,
    pkg_dir: &Path,
    files: &[String],
) -> Result<HashMap<String, PathBuf>, GitFetcherError> {
    let mut out = HashMap::with_capacity(files.len());
    for rel in files {
        let source = pkg_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let bytes = fs::read(&source).map_err(GitFetcherError::Io)?;
        let executable = is_file_executable(&source);
        let (cas_path, _hash) =
            store_dir.write_cas_file(&bytes, executable).map_err(map_write_cas)?;
        out.insert(rel.clone(), cas_path);
    }
    Ok(out)
}

/// `true` when the user-execute bit is set on the on-disk file.
/// POSIX-only; on Windows every file lands as non-executable, matching
/// pnpm v11's behavior where the executable mode flag is meaningful
/// only on POSIX.
pub(crate) fn is_file_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).map(|m| (m.permissions().mode() & 0o100) != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// `true` when a CAS file path encodes "executable" via the `-exec`
/// suffix pnpm's CAFS layout uses. Cheaper than reading filesystem
/// metadata, and matches the write-side encoding in
/// [`pacquet_store_dir::StoreDir::cas_file_path`].
#[cfg(unix)]
fn cas_path_is_executable(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with("-exec"))
}

/// Re-wrap a CAFS write failure as a `GitFetcherError::AddFilesFromDir`.
/// Preserves the miette source chain shape so a future replacement of
/// the per-file `write_cas_file` loop with an `add_files_from_dir`-
/// shaped helper doesn't disturb the dispatcher's error rendering.
pub(crate) fn map_write_cas(err: pacquet_store_dir::WriteCasFileError) -> GitFetcherError {
    let pacquet_store_dir::WriteCasFileError::WriteFile(inner) = err;
    GitFetcherError::AddFilesFromDir(pacquet_store_dir::AddFilesFromDirError::WriteCas(
        pacquet_store_dir::WriteCasFileError::WriteFile(inner),
    ))
}
