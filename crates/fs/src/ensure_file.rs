use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

/// POSIX `EMFILE` — process has hit `RLIMIT_NOFILE`. Hardcoded
/// instead of pulling in `libc` for a single integer that's been
/// stable across every Unix since 4.2BSD.
#[cfg(unix)]
const EMFILE: i32 = 24;

/// POSIX `ENFILE` — system-wide file table is full. Same rationale
/// as [`EMFILE`].
#[cfg(unix)]
const ENFILE: i32 = 23;

/// Run `op`, retrying on `EMFILE` / `ENFILE` with exponential
/// backoff so a transient fd-table exhaustion under heavy
/// concurrency doesn't fail the whole install. Matches pnpm's
/// `graceful-fs` shape — pnpm has run this way for years and the
/// fan-out shape (many concurrent rayon workers each holding fds
/// during CAS extraction + verification) is the same in pacquet.
///
/// Backoff doubles starting at 2 ms and caps at 200 ms; the budget
/// is 32 sleep-and-retry rounds followed by a final attempt (33
/// total calls) for roughly 5–6 s of total wait before we surface
/// the error. Real fd-pressure resolves in tens of ms once other
/// workers finish their writes and close fds, so we hit the cap
/// rarely.
///
/// On Windows the error codes don't map (Win32 returns its own
/// numeric space) and the runtime fd limits work differently, so
/// the helper is a thin pass-through there — the trailing `op()`
/// after the `cfg(unix)` block is the one and only attempt on that
/// platform. Pacquet's Windows build path otherwise stays unchanged.
fn retry_on_fd_pressure<F, T>(mut op: F) -> io::Result<T>
where
    F: FnMut() -> io::Result<T>,
{
    #[cfg(unix)]
    {
        let mut backoff = Duration::from_millis(2);
        for _ in 0..32 {
            match op() {
                Ok(value) => return Ok(value),
                Err(error) if matches!(error.raw_os_error(), Some(EMFILE) | Some(ENFILE)) => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(200));
                }
                Err(error) => return Err(error),
            }
        }
    }
    op()
}

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

    match retry_on_fd_pressure(|| options.open(file_path)) {
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
/// Uses `symlink_metadata` (not `metadata`) first to reject the
/// non-regular-file cases — symlinks in particular. On Unix,
/// `open(O_CREAT|O_EXCL)` returns `EEXIST` even when the dirent is
/// a symlink (POSIX `open` does not follow symlinks under `O_EXCL`),
/// so a tampered / backed-up-and-restored store could route a symlinked
/// dirent into this function. If we fell through directly to `fs::read`
/// (which *does* follow symlinks), a symlink pointing at a file with
/// matching bytes would silently return `Ok(())` without ever
/// materialising a real CAS blob at `file_path`, and downstream
/// `fs::hard_link` on that path would hardlink the symlink itself
/// rather than the target. Scrub instead: `write_atomic`'s `rename`
/// atomically replaces the symlink (or any other non-regular dirent
/// that `rename` can overwrite) with a real regular file. Pnpm v11
/// doesn't guard against this case either, but pacquet's CAS linking
/// path is stricter about file-type than pnpm's, so the guard is
/// worth adding here.
///
/// A `NotFound` on either syscall means the dirent disappeared
/// between our `create_new` attempt and the metadata / read call —
/// another process cleaned it up (unusual, but possible in shared-
/// store setups). Fall through to the atomic-write path, which will
/// re-create it.
fn verify_or_rewrite(
    file_path: &Path,
    content: &[u8],
    mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    match fs::symlink_metadata(file_path) {
        Ok(meta) if !meta.file_type().is_file() => {
            // Symlink, directory, fifo, socket, block/char device —
            // not a regular CAS blob. Scrub via atomic rewrite.
            write_atomic(file_path, content, mode)
        }
        // Cheap size-mismatch reject before we read a single byte —
        // a CAS file whose length doesn't match the buffer we were
        // about to write cannot possibly have matching contents.
        Ok(meta) if meta.len() != content.len() as u64 => write_atomic(file_path, content, mode),
        Ok(_) => match file_equals_bytes(file_path, content) {
            Ok(true) => Ok(()),
            Ok(false) => write_atomic(file_path, content, mode),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                write_atomic(file_path, content, mode)
            }
            Err(error) => {
                Err(EnsureFileError::ReadFile { file_path: file_path.to_path_buf(), error })
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_atomic(file_path, content, mode)
        }
        Err(error) => Err(EnsureFileError::ReadFile { file_path: file_path.to_path_buf(), error }),
    }
}

/// Stream `file_path` and byte-compare against `content` without
/// buffering the whole file in memory.
///
/// `fs::read` (previous shape) allocated a `Vec<u8>` the size of the
/// file; on a CAS entry for a large binary (10–30 MB isn't unusual in
/// `@napi-rs/*`, `esbuild`, etc.) and many concurrent rayon workers
/// hitting this branch, the extra allocation stacked up. Streaming in
/// 8 KB chunks holds a fixed stack buffer regardless of file size.
///
/// Any chunk mismatch returns `Ok(false)` immediately — we don't
/// finish reading the file once we know it differs. An
/// `UnexpectedEof` from `read_exact` is returned as `Ok(false)` too:
/// the file shrunk under us (another process truncated it or the
/// metadata was stale), which by definition means its contents don't
/// match `content`. Other errors propagate.
fn file_equals_bytes(file_path: &Path, content: &[u8]) -> io::Result<bool> {
    use std::io::Read;

    let mut file = retry_on_fd_pressure(|| File::open(file_path))?;
    let mut buf = [0u8; 8 * 1024];
    let mut offset = 0;

    while offset < content.len() {
        let chunk_len = (content.len() - offset).min(buf.len());
        match file.read_exact(&mut buf[..chunk_len]) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(error) => return Err(error),
        }
        if buf[..chunk_len] != content[offset..offset + chunk_len] {
            return Ok(false);
        }
        offset += chunk_len;
    }

    // Confirm the file ends where `content` ends — if there's a
    // trailing byte the size-check earlier missed (shouldn't happen
    // given the size-match guard in `verify_or_rewrite`, but cheap
    // to assert), treat it as not-equal.
    let mut overflow = [0u8; 1];
    match file.read(&mut overflow) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Write `content` to a unique temporary path next to `file_path` and
/// `rename` it over the target. Matches pnpm v11's `writeFileAtomic` +
/// `renameOverwriteSync`. The rename is the only atomic step; an
/// observer sees either the old contents or the new ones, never a
/// half-written blob.
///
/// The temp file itself is opened with `O_CREAT|O_EXCL`
/// (`create_new(true)`) rather than `create+truncate` so we never
/// follow a symlink or truncate a file an attacker (or a crashed
/// prior install) pre-seeded at our predicted temp path. If we hit
/// `AlreadyExists` anyway — collisions are vanishingly rare given the
/// pid + per-process atomic counter temp scheme, but cross-container
/// shared-store setups can re-use pids — we advance the counter and
/// try again, up to `MAX_TEMP_ATTEMPTS` times.
///
/// Open errors are classified as `CreateFile`; write errors as
/// `WriteFile`. On any failure the partially-created temp file is
/// removed best-effort so stale files don't leak into the store
/// shard.
fn write_atomic(
    file_path: &Path,
    content: &[u8],
    #[cfg_attr(windows, allow(unused))] mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    /// Retries after `AlreadyExists` on the temp path. Sixteen fresh
    /// counter values is plenty — under benign conditions we never
    /// collide; under shared-store-across-containers the chance of
    /// 16 consecutive same-pid same-counter collisions is negligible.
    const MAX_TEMP_ATTEMPTS: usize = 16;

    let mut last_already_exists: Option<io::Error> = None;

    for _ in 0..MAX_TEMP_ATTEMPTS {
        let tmp_path = temp_path_for(file_path);

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(mode) = mode {
                options.mode(mode);
            }
        }

        let mut file = match retry_on_fd_pressure(|| options.open(&tmp_path)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // Stale temp file or adversarial / concurrent pre-seed.
                // Retry with a fresh counter; don't touch whatever is
                // at the colliding path.
                last_already_exists = Some(error);
                continue;
            }
            Err(error) => {
                return Err(EnsureFileError::CreateFile { file_path: tmp_path, error });
            }
        };

        if let Err(error) = file.write_all(content) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(EnsureFileError::WriteFile { file_path: tmp_path, error });
        }
        // Close the handle before `rename`. Windows `MoveFileEx` over
        // an open source file can fail with sharing-violation; Unix
        // doesn't care but an early `close` lets the kernel commit
        // dirty buffers before the rename commits the dirent change.
        drop(file);

        if let Err(error) = rename_with_retry(&tmp_path, file_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(EnsureFileError::RenameFile {
                tmp_path,
                file_path: file_path.to_path_buf(),
                error,
            });
        }
        return Ok(());
    }

    // Ran out of temp-name attempts. Surface the last `AlreadyExists`
    // so the operator can see what happened; pick the file_path as
    // the best-effort context since we can't enumerate every temp
    // name we tried.
    Err(EnsureFileError::CreateFile {
        file_path: file_path.to_path_buf(),
        error: last_already_exists.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "exhausted temp-path attempts for atomic CAS rewrite",
            )
        }),
    })
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

/// Classify a `rename` error as transient-retry-worthy.
///
/// On Windows, AV / indexer interference briefly holds the
/// destination open and surfaces as `ERROR_ACCESS_DENIED` (→
/// `PermissionDenied`) or `ERROR_SHARING_VIOLATION` (→
/// `ResourceBusy`, Rust 1.84+ mapping). Both clear on their own
/// within tens-to-hundreds of ms, which is exactly what the retry
/// loop is for.
///
/// On Unix, `rename` returning `EACCES`/`EPERM` is essentially
/// always a permanent permission issue (non-writable directory,
/// sticky-bit conflict, AppArmor deny) — retrying for 60 s just
/// stretches out the failure. `EBUSY` on Unix also tends to be
/// permanent (mount-point conflicts). So on non-Windows the
/// classifier is disabled and any `rename` error propagates
/// immediately.
fn is_transient_rename_error(#[cfg_attr(not(windows), allow(unused))] error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy)
    }
    #[cfg(not(windows))]
    {
        false
    }
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

    /// New-file path: contents land on disk. Mode handling is covered
    /// separately in `unix_mode_is_applied_on_new_files`.
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
    ///
    /// Asserts the **owner** bits specifically rather than the full
    /// `0o777` triplet because `OpenOptionsExt::mode` runs through the
    /// process umask, which strips group / other bits on systems with
    /// a restrictive default (e.g. `umask 0o077` CI shells). Owner
    /// bits are preserved under every sensible umask, so pinning just
    /// those keeps the test robust without weakening what it verifies
    /// (that `mode` is being threaded through to the syscall at all
    /// and that the owner-exec bit survives — the observable property
    /// that distinguishes an executable CAS blob from a data blob).
    #[cfg(unix)]
    #[test]
    fn unix_mode_is_applied_on_new_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let path = tmp.path().join("exec.sh");

        ensure_file(&path, b"#!/bin/sh\n", Some(0o755)).expect("mode-honouring write");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o700;
        assert_eq!(mode, 0o700, "owner rwx bits of 0o755 must survive any reasonable umask",);
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
    /// `PermissionDenied` or `ResourceBusy` and must trigger the
    /// retry loop there. On non-Windows those codes are essentially
    /// always permanent (permission / mount-point issues), so the
    /// classifier must return `false` to avoid pathologically
    /// spinning for 60 s on a misconfigured store dir. Any other
    /// kind must propagate immediately on every platform.
    #[test]
    fn transient_rename_error_classifier() {
        let permission_denied = io::Error::from(io::ErrorKind::PermissionDenied);
        let resource_busy = io::Error::from(io::ErrorKind::ResourceBusy);

        #[cfg(windows)]
        {
            assert!(is_transient_rename_error(&permission_denied));
            assert!(is_transient_rename_error(&resource_busy));
        }
        #[cfg(not(windows))]
        {
            assert!(
                !is_transient_rename_error(&permission_denied),
                "Unix PermissionDenied is permanent, must not retry",
            );
            assert!(
                !is_transient_rename_error(&resource_busy),
                "Unix ResourceBusy is effectively permanent, must not retry",
            );
        }

        // Non-transient kinds must never trigger the retry loop on
        // any platform — a regression classifying e.g. `NotFound` as
        // transient would spin for 60 s on a legitimately missing
        // source.
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

    /// A symlink at the target path — which on Unix returns `EEXIST`
    /// from `open(O_CREAT|O_EXCL)` just like a regular file would —
    /// must be scrubbed and replaced with a real regular file even
    /// when its target's bytes match what we were about to write.
    /// Leaving the symlink in place would fool downstream
    /// `fs::hard_link` (which hardlinks the symlink itself on Linux,
    /// not the target) and leak non-regular dirents into the CAS.
    #[cfg(unix)]
    #[test]
    fn symlink_at_cas_path_is_scrubbed_to_a_regular_file() {
        let tmp = tempdir().unwrap();
        let real_target = tmp.path().join("other_real_file");
        fs::write(&real_target, b"payload").unwrap();

        let cas_path = tmp.path().join("cas_entry");
        std::os::unix::fs::symlink(&real_target, &cas_path).unwrap();

        ensure_file(&cas_path, b"payload", None).expect("symlink should be scrubbed");

        let meta = fs::symlink_metadata(&cas_path).unwrap();
        assert!(
            meta.file_type().is_file(),
            "cas_path must be a regular file after scrub, got {:?}",
            meta.file_type(),
        );
        assert_eq!(fs::read(&cas_path).unwrap(), b"payload");
        // The file the symlink used to point at is untouched — we
        // replaced the link, not followed it.
        assert_eq!(fs::read(&real_target).unwrap(), b"payload");
    }

    /// Dangling symlink (points nowhere) is also scrubbed to a real
    /// file via the same `symlink_metadata` guard. Without the guard
    /// we'd still end up in `write_atomic` via the `NotFound` branch
    /// on `fs::read`, but this pins the expected control flow.
    #[cfg(unix)]
    #[test]
    fn dangling_symlink_at_cas_path_is_scrubbed_to_a_regular_file() {
        let tmp = tempdir().unwrap();
        let cas_path = tmp.path().join("cas_entry");
        std::os::unix::fs::symlink(tmp.path().join("nonexistent"), &cas_path).unwrap();

        ensure_file(&cas_path, b"fresh", None).expect("dangling link should be scrubbed");

        let meta = fs::symlink_metadata(&cas_path).unwrap();
        assert!(meta.file_type().is_file(), "cas_path must end as a regular file");
        assert_eq!(fs::read(&cas_path).unwrap(), b"fresh");
    }

    /// Happy-path rename (no transient errors) moves the payload
    /// atomically and removes the source. Correctness only — we
    /// deliberately don't assert a wall-clock bound because rename
    /// latency on loaded CI / slow filesystems can exceed any
    /// reasonable timing threshold without the retry path actually
    /// being taken.
    #[test]
    fn rename_with_retry_succeeds_when_no_error() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("dst");
        fs::write(&src, b"payload").unwrap();

        rename_with_retry(&src, &dst).expect("rename should succeed");

        assert_eq!(fs::read(&dst).unwrap(), b"payload");
        assert!(!src.exists(), "source should be gone after rename");
    }

    /// Streaming byte-compare returns `true` iff the file on disk is
    /// identical to `content`. Pins the three cases
    /// `verify_or_rewrite` routes through it: exact match (skip
    /// path), same length but different bytes (atomic rewrite),
    /// different length (atomic rewrite).
    #[test]
    fn file_equals_bytes_classifies_match_mismatch_and_length_mismatch() {
        let tmp = tempdir().unwrap();

        let equal = tmp.path().join("equal");
        fs::write(&equal, b"hello world").unwrap();
        assert!(file_equals_bytes(&equal, b"hello world").unwrap());

        let content_diff = tmp.path().join("content_diff");
        fs::write(&content_diff, b"hello world").unwrap();
        assert!(!file_equals_bytes(&content_diff, b"hello WORLD").unwrap());

        // `verify_or_rewrite`'s size-check short-circuits before
        // reaching this function in practice, but the function
        // itself still has to classify correctly if called directly.
        let length_diff = tmp.path().join("length_diff");
        fs::write(&length_diff, b"short").unwrap();
        assert!(!file_equals_bytes(&length_diff, b"longer payload").unwrap());
    }

    /// Multi-chunk files exercise the inner `read_exact` loop rather
    /// than landing entirely in the first 8 KB read. Guards against
    /// off-by-one regressions in the chunk-offset math, and confirms
    /// a byte flipped in the *last* chunk isn't masked by an early
    /// "first-chunk-matched" short-circuit.
    #[test]
    fn file_equals_bytes_handles_multi_chunk_files() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("big");

        // 20 KB: at least three 8 KB chunks.
        let content: Vec<u8> = (0..20_000).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &content).unwrap();

        assert!(file_equals_bytes(&path, &content).unwrap());

        let mut perturbed = content.clone();
        *perturbed.last_mut().unwrap() ^= 0xff;
        assert!(!file_equals_bytes(&path, &perturbed).unwrap());
    }

    /// Transient `EMFILE` / `ENFILE` failures must be retried until
    /// the underlying op succeeds. Cell-counts attempts so we can
    /// pin both the predicate ("retries on these errnos") and the
    /// loop control flow ("returns the first `Ok` value the closure
    /// produces").
    #[cfg(unix)]
    #[test]
    fn retry_on_fd_pressure_retries_emfile_and_enfile_until_success() {
        for errno in [EMFILE, ENFILE] {
            let attempts = std::cell::Cell::new(0);
            let result = retry_on_fd_pressure(|| {
                let attempt = attempts.get();
                attempts.set(attempt + 1);
                if attempt < 2 {
                    Err(io::Error::from_raw_os_error(errno))
                } else {
                    Ok("ok")
                }
            });
            assert_eq!(result.unwrap(), "ok");
            assert_eq!(attempts.get(), 3, "errno {errno} should have been retried twice");
        }
    }

    /// Errors that aren't fd-pressure must propagate immediately —
    /// retrying would just delay surfacing a real failure (e.g. a
    /// genuine `NotFound` on the parent dir).
    #[cfg(unix)]
    #[test]
    fn retry_on_fd_pressure_propagates_non_fd_errors() {
        let attempts = std::cell::Cell::new(0);
        let result: io::Result<()> = retry_on_fd_pressure(|| {
            attempts.set(attempts.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts.get(), 1, "non-fd-pressure errors must not retry");
    }
}
