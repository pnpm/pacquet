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
    fs, io,
    path::{Component, Path, PathBuf},
};

/// Safely join a relative path onto a trusted root.
///
/// Rejects anything that wouldn't stay under `root`:
///
/// - Absolute paths (`/etc/passwd`, `C:\foo`, etc.) — refuse.
/// - `..` / root / drive-prefix components — refuse.
/// - `.` components — silently dropped.
/// - Normal segments — pushed onto `root` one at a time.
///
/// Both `materialize_into` and `import_into_cas` receive their
/// relative paths from the install dispatcher's `cas_paths` map,
/// which traces back to either a tarball extraction or a packlist
/// over a freshly-checked-out git tree. Tarball entries on the
/// extraction side already get path-traversal guards in
/// `pacquet-tarball`, but defense-in-depth at this layer means a
/// future caller (or a bug in the upstream sanitiser) can't turn
/// a malformed entry into a write outside the working tree.
fn join_checked(root: &Path, rel: &str) -> Result<PathBuf, GitFetcherError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(GitFetcherError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("absolute path is not allowed in CAS entry: {rel}"),
        )));
    }
    let mut out = root.to_path_buf();
    for c in rel_path.components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(GitFetcherError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("non-normal path component in CAS entry: {rel}"),
                )));
            }
        }
    }
    Ok(out)
}

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
        // `rel` uses forward slashes regardless of host platform.
        // `Path::components()` (called inside `join_checked`)
        // recognises both `/` and `\` as separators on Windows, so we
        // can hand `rel` over directly and avoid a per-file `String`
        // allocation.
        let target = join_checked(target_dir, rel)?;
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
        // See the matching note in `materialize_into`: `join_checked`
        // accepts forward-slash relative paths verbatim on every host.
        let source = join_checked(pkg_dir, rel)?;
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

#[cfg(test)]
mod tests {
    use super::{GitFetcherError, join_checked, materialize_into};
    use pacquet_store_dir::StoreDir;
    use std::{collections::HashMap, io, path::Path};
    use tempfile::tempdir;

    fn assert_invalid_input(err: GitFetcherError) {
        match err {
            GitFetcherError::Io(io_err) => {
                assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected Io(InvalidInput), got {other:?}"),
        }
    }

    #[test]
    fn join_checked_accepts_normal_segments() {
        let root = Path::new("/root");
        let joined = join_checked(root, "a/b/c.txt").unwrap();
        // Use components() so the assertion stays platform-agnostic.
        let expected: Vec<_> = Path::new("/root/a/b/c.txt").components().collect();
        let actual: Vec<_> = joined.components().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn join_checked_strips_current_dir_components() {
        // `./a` and `a` both produce the same `<root>/a` — leading
        // `./` is a no-op, matching upstream's `path.normalize`.
        let root = Path::new("/root");
        let joined = join_checked(root, "./a").unwrap();
        let expected: Vec<_> = Path::new("/root/a").components().collect();
        let actual: Vec<_> = joined.components().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn join_checked_rejects_absolute_paths() {
        assert_invalid_input(join_checked(Path::new("/root"), "/etc/passwd").unwrap_err());
    }

    #[test]
    fn join_checked_rejects_parent_dir() {
        assert_invalid_input(join_checked(Path::new("/root"), "../escape").unwrap_err());
        // Even a `..` deep in the path must be refused — otherwise
        // `a/../../escape` would slip through.
        assert_invalid_input(join_checked(Path::new("/root"), "a/../escape").unwrap_err());
    }

    #[test]
    fn materialize_into_rejects_traversal() {
        // The dispatcher must never write a file outside `target_dir`
        // even when handed a malicious `cas_paths` map. Build one
        // with a `..` entry and confirm we get InvalidInput.
        let target = tempdir().unwrap();
        let cas_root = tempdir().unwrap();
        let store_dir = StoreDir::from(cas_root.path().to_path_buf());
        let (cas_path, _hash) = store_dir.write_cas_file(b"poison\n", false).unwrap();

        let mut bad: HashMap<String, _> = HashMap::new();
        bad.insert("../escape".to_string(), cas_path);

        let err = materialize_into(&bad, target.path()).unwrap_err();
        assert_invalid_input(err);
        // The `escape` file must not exist anywhere — neither in the
        // target dir nor in its parent.
        assert!(!target.path().join("escape").exists());
        assert!(!target.path().parent().unwrap().join("escape").exists());
    }
}
