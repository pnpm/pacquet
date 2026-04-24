use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
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
    #[display("Failed to read existing file at {file_path:?}: {error}")]
    ReadFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to rename {tmp_path:?} over {file_path:?}: {error}")]
    RenameFile {
        tmp_path: PathBuf,
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

/// Write `content` to `file_path` with pnpm v11's `writeBufferToCafs`
/// semantics.
///
/// The parent directory must already exist. Callers that can't
/// guarantee that should call [`ensure_parent_dir`] first — splitting
/// the two lets the CAFS writer share one `create_dir_all` per shard
/// instead of paying it per file.
///
/// Sequence (ports `store/cafs/src/writeBufferToCafs.ts` +
/// `store/cafs/src/writeFile.ts` on pnpm v11):
///
/// 1. Try `O_CREAT | O_EXCL` open (`OpenOptions::create_new(true)`).
///    On success we own the file and write `content` directly.
/// 2. On `ErrorKind::AlreadyExists` (warm cache or concurrent writer
///    race) re-read the file and byte-compare with `content`. CAS
///    paths are hash-derived, so matching bytes == matching digest;
///    this is the pacquet-specific equivalent of pnpm's
///    `verifyFileIntegrity(fileDest, integrity)` — we already have
///    the expected bytes in hand, so we skip the extra hash step.
/// 3. If bytes match → `Ok(())`. The file is a live CAS entry; leaving
///    it alone is correct and matches pnpm's `Date.now()` return there.
/// 4. If bytes mismatch, a prior install crashed mid-write and left a
///    torn blob. Recover by writing a fresh temp file next to the
///    target and `rename`ing it over. Rename is atomic on Unix
///    (`rename(2)`) and replaces-in-place on Windows
///    (`SetFileInformationByHandle`/`MoveFileEx`), so an observer
///    never sees a partial file. Matches pnpm's `writeFileAtomic` +
///    `renameOverwriteSync`.
/// 5. Any other open error propagates as `CreateFile`.
///
/// Differences from pnpm v11's shape, deliberate:
///
/// * **No upfront `stat`**: pnpm stats first so it can skip directly
///   to `verifyFileIntegrity` on exists. We skip the stat and rely on
///   the `create_new`/`AlreadyExists` signal, which saves one syscall
///   per file on cold installs (where every file is new) at the cost
///   of a slightly different path ordering on warm hits.
/// * **Byte-compare instead of `crypto.hash`**: we already have the
///   buffer we were about to write, so comparing against it
///   implicitly verifies the sha512 without a second hash pass. Same
///   correctness guarantee, one fewer full-buffer walk.
/// * **No `locker: Map<string, number>` process-local cache**: pnpm's
///   locker skips re-verifying the same file within one install.
///   Pacquet's hot path calls `ensure_file` at most once per CAS file
///   per install (the `StoreIndex` cache decides whether we even get
///   here), so the locker would be mostly empty work. Can revisit if
///   profiling shows repeated AlreadyExists hits on a single path.
///
/// Matches pnpm's guarantee: a successful return means `file_path`
/// exists on disk with contents equal to `content`. A torn mid-write
/// from a previous install is self-healing, not persistent.
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
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_or_rewrite(file_path, content, mode)
        }
        Err(error) => {
            Err(EnsureFileError::CreateFile { file_path: file_path.to_path_buf(), error })
        }
    }
}

/// Re-read an already-present CAS file and byte-compare with `content`.
/// If they match we're done; if not, recover the torn blob by writing a
/// fresh temp file and renaming it over the target.
///
/// A `NotFound` on the re-read means the file disappeared between our
/// `create_new` attempt and the `read` — another process cleaned it up
/// (unusual, but possible in shared-store setups). Fall through to the
/// atomic-write path, which will re-create it.
fn verify_or_rewrite(
    file_path: &Path,
    content: &[u8],
    mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    match fs::read(file_path) {
        Ok(existing) if existing == content => Ok(()),
        Ok(_) => write_atomic(file_path, content, mode),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_atomic(file_path, content, mode)
        }
        Err(error) => Err(EnsureFileError::ReadFile { file_path: file_path.to_path_buf(), error }),
    }
}

/// Write `content` to a unique temporary path next to `file_path` and
/// `rename` it over the target. Matches pnpm v11's `writeFileAtomic` +
/// `renameOverwriteSync`. The rename is the only atomic step; an
/// observer sees either the old contents or the new ones, never a
/// half-written blob.
///
/// If either the write or the rename fails we best-effort remove the
/// temp file to avoid leaking stale files into the store shard.
fn write_atomic(
    file_path: &Path,
    content: &[u8],
    #[cfg_attr(windows, allow(unused))] mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    let tmp_path = temp_path_for(file_path);

    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(mode) = mode {
            options.mode(mode);
        }
    }

    let write_result = options.open(&tmp_path).and_then(|mut file| file.write_all(content));

    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(EnsureFileError::WriteFile { file_path: tmp_path, error });
    }

    if let Err(error) = rename_with_retry(&tmp_path, file_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(EnsureFileError::RenameFile {
            tmp_path,
            file_path: file_path.to_path_buf(),
            error,
        });
    }

    Ok(())
}

/// Total budget for retrying a rename that keeps hitting transient
/// errors. Matches pnpm's `rename-overwrite` retry window.
const RENAME_RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Cap on per-iteration sleep — pnpm grows the backoff by 10 ms each
/// loop and stops growing at 100 ms.
const RENAME_RETRY_BACKOFF_CAP: Duration = Duration::from_millis(100);

/// `fs::rename` with the one retry family that actually hits pacquet
/// in practice: Windows Defender (and other Windows antivirus / file-
/// indexer tooling) momentarily holding the destination open, which
/// makes the rename fail with `ERROR_ACCESS_DENIED` /
/// `ERROR_SHARING_VIOLATION`. These surface through Rust's
/// `io::ErrorKind` as `PermissionDenied` or `ResourceBusy`, and they
/// clear as soon as the scan completes — a short sleep + retry
/// recovers. Mirrors the `EPERM|EACCES|EBUSY` arm of
/// `rename-overwrite`'s `renameOverwriteSync` (see zkochan/packages/
/// rename-overwrite/index.js): 60-second total budget, 10 ms backoff
/// step, 100 ms cap.
///
/// Other retry arms from `rename-overwrite` (`ENOTEMPTY`/`EEXIST`/
/// `ENOTDIR` swap-rename, `ENOENT` mkdir-and-recurse, `EXDEV` copy-
/// and-delete) don't apply to this call site: temp and target share
/// the CAS shard dir (already pre-created by `StoreDir::init`), both
/// are files not directories, and pacquet's CAS readers
/// (`link_file` → `fs::hard_link` / `reflink_copy`) don't keep file
/// handles on the target, so there's no "parallel reader sees a gap"
/// concern that would motivate swap-rename.
fn rename_with_retry(src: &Path, dst: &Path) -> io::Result<()> {
    let mut backoff = Duration::ZERO;
    let start = Instant::now();

    loop {
        match fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if !is_transient_rename_error(&error) || start.elapsed() >= RENAME_RETRY_BUDGET {
                    return Err(error);
                }
                if !backoff.is_zero() {
                    std::thread::sleep(backoff);
                }
                backoff = (backoff + Duration::from_millis(10)).min(RENAME_RETRY_BACKOFF_CAP);
            }
        }
    }
}

/// Windows AV / indexer interference surfaces as `PermissionDenied`
/// (`ERROR_ACCESS_DENIED`) or `ResourceBusy`
/// (`ERROR_SHARING_VIOLATION`). `ResourceBusy` is the Rust 1.84+
/// mapping; on older toolchains it came through as `Other` instead,
/// but pacquet's MSRV is well past that. Unix never hits these codes
/// for our same-directory file rename, so the retry loop is
/// effectively a no-op there.
fn is_transient_rename_error(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy)
}

/// Build a unique temp path next to `file_path`. Mirrors pnpm v11's
/// `pathTemp` in spirit: `{stripped_basename}{pid}{counter}`. The
/// counter is a process-local monotonically-increasing `AtomicU64`,
/// giving uniqueness across rayon / tokio workers in the same process;
/// combining it with the pid avoids collisions when multiple install
/// processes share a store dir.
///
/// We drop `-exec` / any dash-suffix the same way pnpm's `removeSuffix`
/// does, mainly so temp files don't look like executable CAS entries
/// to any observer scanning the shard.
fn temp_path_for(file_path: &Path) -> PathBuf {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();

    let parent = file_path.parent().unwrap_or_else(|| Path::new("."));
    let name = file_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let base = strip_dash_suffix(&name);

    parent.join(format!("{base}{pid}{counter}"))
}

/// Port of pnpm's `removeSuffix` from `store/cafs/src/writeBufferToCafs.ts`:
/// strip the first `-…` tail; if the tail was `-exec`, append `x`. On
/// pacquet's CAS names (`{hex}` or `{hex}-exec`) the only real input is
/// those two shapes, but we stay faithful to the general form so any
/// future suffix landing upstream doesn't silently diverge.
fn strip_dash_suffix(name: &str) -> String {
    let Some(dash_pos) = name.find('-') else {
        return name.to_string();
    };
    let without_suffix = &name[..dash_pos];
    if &name[dash_pos..] == "-exec" {
        format!("{without_suffix}x")
    } else {
        without_suffix.to_string()
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

    /// Pre-existing file with matching content short-circuits as
    /// `Ok(())` and does not touch the target. Mirrors pnpm v11's
    /// `verifyFileIntegrity(fileDest, integrity) === true` branch in
    /// `writeBufferToCafs.ts`.
    #[test]
    fn existing_target_with_matching_content_is_preserved() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("existing.txt");
        fs::write(&path, b"same").unwrap();

        ensure_file(&path, b"same", None).expect("matching contents should short-circuit");

        assert_eq!(fs::read(&path).unwrap(), b"same");
    }

    /// Pre-existing file with *wrong* contents is a torn-blob case and
    /// must be atomically replaced with the buffer we were trying to
    /// write. Mirrors the `writeFileAtomic` branch pnpm takes when
    /// `verifyFileIntegrity` fails.
    #[test]
    fn existing_target_with_wrong_content_is_overwritten_atomically() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("torn.txt");
        fs::write(&path, b"garbage-from-crashed-prior-install").unwrap();

        ensure_file(&path, b"fresh", None).expect("torn blob should be rewritten");

        assert_eq!(fs::read(&path).unwrap(), b"fresh");
        // No leftover temp files in the same directory.
        let siblings: Vec<_> =
            fs::read_dir(tmp.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(siblings, vec![std::ffi::OsString::from("torn.txt")]);
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

    /// `-exec` suffix becomes `x` in the temp name (pnpm `removeSuffix`
    /// parity). Pins the naming scheme so future tweaks stay explicit.
    #[test]
    fn temp_path_strips_exec_suffix() {
        let p = Path::new("/tmp/store/v11/files/ab/cdef-exec");
        let tmp = temp_path_for(p);
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("cdefx"), "got {name}");
    }

    /// Plain hex basenames go through untouched apart from the pid +
    /// counter suffix.
    #[test]
    fn temp_path_passes_plain_basename_through() {
        let p = Path::new("/tmp/store/v11/files/ab/cdef");
        let tmp = temp_path_for(p);
        let name = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("cdef"), "got {name}");
        assert_ne!(name, "cdef", "must include pid + counter suffix");
    }

    /// Windows AV / indexer interference surfaces as
    /// `PermissionDenied` or `ResourceBusy`; both must trigger the
    /// retry loop. Any other kind must propagate immediately.
    #[test]
    fn transient_rename_error_classifier() {
        assert!(is_transient_rename_error(&io::Error::from(io::ErrorKind::PermissionDenied)));
        assert!(is_transient_rename_error(&io::Error::from(io::ErrorKind::ResourceBusy)));

        // Non-transient kinds: must not trigger the retry loop. A
        // regression that classified e.g. `NotFound` as transient
        // would pathologically spin for 60 seconds on a legitimately
        // missing source.
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::AlreadyExists,
            io::ErrorKind::InvalidInput,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Unsupported,
            io::ErrorKind::Other,
        ] {
            assert!(
                !is_transient_rename_error(&io::Error::from(kind)),
                "{kind:?} must not be classified as transient"
            );
        }
    }

    /// Happy-path rename (no transient errors) returns immediately
    /// via the `Ok(())` branch without sleeping. Pins that the retry
    /// loop doesn't accidentally delay successful renames.
    #[test]
    fn rename_with_retry_succeeds_fast_when_no_error() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::write(&src, b"payload").unwrap();

        let start = Instant::now();
        rename_with_retry(&src, &dst).expect("rename should succeed");
        let elapsed = start.elapsed();

        assert_eq!(fs::read(&dst).unwrap(), b"payload");
        assert!(!src.exists(), "source should be gone after rename");
        // The retry loop's minimum sleep is 10 ms; a successful
        // first attempt must not hit that, so elapsed << 10 ms.
        assert!(elapsed < Duration::from_millis(10), "took {elapsed:?}");
    }
}
