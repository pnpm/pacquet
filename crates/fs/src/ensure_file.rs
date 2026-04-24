use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Error type of [`ensure_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum EnsureFileError {
    #[display("Failed to create the parent directory at {parent_dir:?}: {error}")]
    CreateDir {
        parent_dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to create file at {file_path:?}: {error}")]
    CreateFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to write to file at {file_path:?}: {error}")]
    WriteFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Ensure `dir` (and any missing ancestors) exists. Idempotent.
///
/// Split out from [`ensure_file`] so hot-path callers (the CAFS writer)
/// can cache which directories they've already created and skip the
/// syscall cost when they have — `fs::create_dir_all` does a `stat` on
/// every call even when the directory already exists, which adds up to
/// one wasted `stat` per file on a cold install.
pub fn ensure_parent_dir(dir: &Path) -> Result<(), EnsureFileError> {
    fs::create_dir_all(dir)
        .map_err(|error| EnsureFileError::CreateDir { parent_dir: dir.to_path_buf(), error })
}

/// Write `content` to `file_path` unless it already exists.
///
/// **The parent directory must already exist.** Callers that can't
/// guarantee that should call [`ensure_parent_dir`] first — splitting
/// the two lets the CAFS writer share one `create_dir_all` per shard
/// instead of paying it per file.
///
/// Uses `O_CREAT | O_EXCL` (via [`OpenOptions::create_new`]), mirroring
/// pnpm v11's `writeFileExclusive` in `store/cafs/src/writeFile.ts`. A
/// pre-existing target is swallowed as `ErrorKind::AlreadyExists` →
/// `Ok(())`, which is correct for both the warm-cache case (the file is
/// already at the hash-derived path so its contents are by definition
/// correct) and the concurrent-writer race (another install process on
/// the same store raced to create the same CAS entry — again, hash-
/// keyed path means contents match). The upstream equivalent is
/// `writeBufferToCafs.ts`'s `err.code === 'EEXIST'` branch, minus the
/// integrity re-verification that pnpm does there: pacquet doesn't
/// re-verify individual CAS files on write because (a) the path itself
/// is the integrity assertion and (b) the tarball-level ssri check has
/// already passed for the batch these bytes came from. Torn blobs left
/// by a crashed mid-write remain an open concern tracked separately in
/// `investigations/pacquet-macos-perf.md` §5 — the fix there is
/// temp-file + rename, orthogonal to this syscall shape.
///
/// Saves the upfront `file_path.exists()` stat that the pre-pnpm-v11
/// shape of this function paid on every call; on a cold install where
/// most files are new, that stat always returned `false` and was pure
/// waste.
pub fn ensure_file(
    file_path: &Path,
    content: &[u8],
    #[cfg_attr(windows, allow(unused))] mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(mode) = mode {
            options.mode(mode);
        }
    }

    match options.open(file_path) {
        Ok(mut file) => file.write_all(content).map_err(|error| EnsureFileError::WriteFile {
            file_path: file_path.to_path_buf(),
            error,
        }),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(EnsureFileError::CreateFile { file_path: file_path.to_path_buf(), error })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// New-file path: contents land on disk with the requested mode.
    #[test]
    fn writes_a_new_file() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("new.txt");

        ensure_file(&path, b"hello", None).expect("new-file write succeeds");

        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    /// Pre-existing file short-circuits as `Ok(())` and — crucially —
    /// does not overwrite the existing contents. Mirrors pnpm v11's
    /// `EEXIST` branch in `writeBufferToCafs.ts`: the CAS path already
    /// asserts the bytes, so leaving the file alone is correct.
    #[test]
    fn existing_target_is_preserved() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("existing.txt");
        fs::write(&path, b"old").unwrap();

        ensure_file(&path, b"new", None).expect("existing target short-circuits");

        assert_eq!(
            fs::read(&path).unwrap(),
            b"old",
            "ensure_file must never silently overwrite an existing file",
        );
    }

    /// Missing parent directory surfaces as a `CreateFile` error
    /// (kind `NotFound`). Callers are expected to `ensure_parent_dir`
    /// first; this pins that contract so a regression that quietly
    /// created ancestors would fail the test.
    #[test]
    fn missing_parent_dir_errors() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("nested/does/not/exist/file.txt");

        let err = ensure_file(&path, b"x", None).expect_err("missing parent should fail");
        match err {
            EnsureFileError::CreateFile { error, .. } => {
                assert_eq!(error.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("expected CreateFile/NotFound, got {other:?}"),
        }
    }

    /// Unix mode is honoured on the new-file path. Skipped on Windows
    /// where the `mode` argument is `#[cfg_attr(windows, allow(unused))]`.
    #[cfg(unix)]
    #[test]
    fn unix_mode_is_applied_on_new_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let path = tmp.path().join("exec.sh");

        ensure_file(&path, b"#!/bin/sh\n", Some(0o755)).expect("mode-honouring write");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}
