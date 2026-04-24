use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU8, Ordering},
};

/// Error type for [`link_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkFileError {
    #[display("cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    // `link_file` now dispatches to copy / reflink / hardlink depending
    // on `PackageImportMethod`, so a "fail to create a link" message
    // would be misleading when the configured method is `Copy`. Using
    // pnpm's "import" terminology (see `createPackageImporter`) so the
    // message is accurate regardless of which tier actually ran.
    #[display("failed to import {from:?} to {to:?}: {error}")]
    Import {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("failed to remove stale dirent at {path:?}: {error}")]
    RemoveStale {
        path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

// Cached downgrade states shared by `Auto` and `CloneOrCopy`.
//
// This cache is process-global, not keyed by `(source fs, target fs)`.
// Once we observe a tier failing anywhere, we stop trying it for the
// rest of the process. That's a coarse optimization to avoid paying
// the "try reflink, fail" cost for every file in installs where a
// higher tier is not usable on the store / workspace pair.
//
// A failure on one path can therefore downgrade later calls that
// would have succeeded on a different pair — in practice pacquet runs
// one install per process with one store and one target root, so this
// is fine. Pnpm's per-importer `let auto` closure (see
// `render-peer/fs/indexed-pkg-importer/src/index.ts`,
// `createAutoImporter` / `createCloneOrCopyImporter`) has the same
// coarseness once `pnpm install` has picked an import direction.
//
// The state is monotonic (`CLONE` → `HARDLINK` → `COPY`) and updated
// with `fetch_max`, so concurrent rayon workers racing on the first
// failure all converge to the same downgraded value without a lock.
// Worst case cost on startup is `N` stale attempts per tier where `N`
// is the rayon thread count — bounded, not per-file.
const LINK_STATE_CLONE: u8 = 0;
const LINK_STATE_HARDLINK: u8 = 1;
const LINK_STATE_COPY: u8 = 2;

// One-shot "we picked this import method" log, matching pnpm's
// `packageImportMethodLogger.debug({ method: 'clone' | 'hardlink' | 'copy' })`
// in `fs/indexed-pkg-importer/src/index.ts`. Emits once per process per
// method so a reader of the logs can tell which tier actually ran —
// crucial for verifying hardlinks are kicking in on CI runners where
// reflink isn't available.
//
// Pnpm logs at `debug`; pacquet uses `info` so the message surfaces
// without verbose logging configured. `fetch_or` returns the previous
// bitfield, so the first caller to set a given bit is the one that
// emits.
const LOG_FLAG_CLONE: u8 = 1 << 0;
const LOG_FLAG_HARDLINK: u8 = 1 << 1;
const LOG_FLAG_COPY: u8 = 1 << 2;
static LOGGED_METHODS: AtomicU8 = AtomicU8::new(0);

fn log_method_once(flag: u8, method: &'static str) {
    if LOGGED_METHODS.fetch_or(flag, Ordering::Relaxed) & flag == 0 {
        tracing::info!(target: "pacquet::package_import_method", method);
    }
}

/// Materialize a CAFS file into `target_link` using `method`.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    method: PackageImportMethod,
    source_file: &Path,
    target_link: &Path,
) -> Result<(), LinkFileError> {
    // If the target resolves to a live file (directly or via a
    // symlink), a prior install placed it and there's nothing to do.
    // `fs::metadata` follows symlinks and returns `Err(NotFound)` for
    // dangling ones, which is exactly what we want here.
    if fs::metadata(target_link).is_ok() {
        return Ok(());
    }
    // `metadata` above can also fail when the dirent itself is a
    // dangling symlink — left behind by an interrupted prior install.
    // `symlink_metadata` doesn't follow, so it'll succeed in that
    // case. Scrub the broken link so the subsequent link / copy
    // doesn't collide with `AlreadyExists` and so the installed
    // package isn't left with a silently-missing file.
    if let Ok(meta) = fs::symlink_metadata(target_link) {
        if meta.file_type().is_symlink() {
            fs::remove_file(target_link).map_err(|error| LinkFileError::RemoveStale {
                path: target_link.to_path_buf(),
                error,
            })?;
        }
        // Non-symlink dirent present but `metadata` failed — rare
        // (permissions, stale NFS handle, …). Let the link / copy
        // below surface the specific error rather than guess at a
        // recovery here.
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| LinkFileError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    // Hardlinking a file from the store into `node_modules` means any
    // package that edits its own files at runtime (postinstall scripts
    // are the usual offender) ends up mutating the shared store copy.
    // Current pnpm's indexed-pkg-importer does not guard against this
    // either — postinstall handling lives in the script runner, not the
    // import layer — so there's nothing to gate on here.
    let result = match method {
        PackageImportMethod::Auto => {
            static AUTO_STATE: AtomicU8 = AtomicU8::new(LINK_STATE_CLONE);
            auto_link(&AUTO_STATE, source_file, target_link)
        }
        // pnpm's explicit `hardlink` method uses `hardlinkPkg(linkOrCopy)`
        // which falls back to copy on `EXDEV` (cross-device link not
        // permitted) but propagates other errors. Match that: if the
        // user asks for hardlink and they've put their store on a
        // different device from `node_modules`, copy silently; anything
        // else (missing source, permission denied, …) is a real error
        // and should surface. No caching — the `fs::hard_link` syscall
        // itself is already cheap; pnpm doesn't cache this path either.
        PackageImportMethod::Hardlink => match fs::hard_link(source_file, target_link) {
            Ok(()) => {
                log_method_once(LOG_FLAG_HARDLINK, "hardlink");
                Ok(())
            }
            Err(error) if is_cross_device(&error) => {
                log_method_once(LOG_FLAG_COPY, "copy");
                fs::copy(source_file, target_link).map(drop)
            }
            Err(error) => Err(error),
        },
        PackageImportMethod::Clone => {
            reflink_copy::reflink(source_file, target_link).inspect(|_| {
                log_method_once(LOG_FLAG_CLONE, "clone");
            })
        }
        PackageImportMethod::CloneOrCopy => {
            static CLONE_OR_COPY_STATE: AtomicU8 = AtomicU8::new(LINK_STATE_CLONE);
            clone_or_copy_link(&CLONE_OR_COPY_STATE, source_file, target_link)
        }
        PackageImportMethod::Copy => {
            log_method_once(LOG_FLAG_COPY, "copy");
            fs::copy(source_file, target_link).map(drop)
        }
    };

    result.map_err(|error| LinkFileError::Import {
        from: source_file.to_path_buf(),
        to: target_link.to_path_buf(),
        error,
    })
}

/// EXDEV = "cross-device link not permitted". Linux / macOS / BSD all
/// use errno 18; Windows maps its equivalent `ERROR_NOT_SAME_DEVICE`
/// to raw OS error 17 in Rust's `io::Error`. pnpm detects this by
/// checking `err.message.startsWith('EXDEV: cross-device link not
/// permitted')` — we can be a little tighter by looking at the raw
/// errno.
fn is_cross_device(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(18) | Some(17))
}

/// Errors that indicate "this filesystem can't reflink" — the kernel
/// rejected the ioctl, not the file. The downgrade cache uses this to
/// skip the reflink tier for subsequent files, while letting
/// `NotFound` / `PermissionDenied` / `AlreadyExists` and other real
/// errors propagate (a one-off missing-source file must not
/// permanently disable reflink for the rest of the process).
///
/// `ErrorKind::Unsupported` covers Rust's portable layer when newer
/// standards lands; the raw-errno list covers platforms where the
/// kernel reports the capability gap as a bare `Err(_)` without
/// mapping to a kind:
/// * `ENOSYS` (38) — syscall not implemented
/// * `EOPNOTSUPP` / `ENOTSUP` (95 Linux, 102 FreeBSD, 45 macOS) —
///   operation not supported on this fd
/// * `ENOTTY` (25) — what ext4 returns for `ioctl_ficlone` when the
///   filesystem doesn't implement reflink; without this, ext4 would
///   never downgrade and every file would pay the failed-ioctl cost
///
/// We additionally treat `EXDEV` as a fallback trigger — a cross-device
/// reflink can't possibly succeed no matter how many times we retry.
fn is_reflink_fallback_error(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::Unsupported)
        || is_cross_device(err)
        || matches!(err.raw_os_error(), Some(38) | Some(95) | Some(102) | Some(45) | Some(25))
}

/// Errors that indicate "this filesystem / device pair can't
/// hardlink". In practice this is `EXDEV` (cross-device) and
/// `ErrorKind::Unsupported` (some exotic FSes refuse hardlinks
/// altogether). Everything else propagates.
fn is_hardlink_fallback_error(err: &io::Error) -> bool {
    is_cross_device(err) || matches!(err.kind(), io::ErrorKind::Unsupported)
}

/// `Auto`'s clone → hardlink → copy chain, using `state` to skip tiers
/// that have already failed in this process. Factored out so tests can
/// pass their own `AtomicU8` and exercise the downgrade logic in
/// isolation — the production path uses a `static` declared inside
/// [`link_file`]. Only capability / cross-device style failures
/// downgrade the cached state; other errors propagate immediately so a
/// one-off `NotFound` on a single file doesn't permanently disable a
/// tier for the rest of the process.
fn auto_link(state: &AtomicU8, source: &Path, target: &Path) -> io::Result<()> {
    loop {
        match state.load(Ordering::Relaxed) {
            LINK_STATE_CLONE => match reflink_copy::reflink(source, target) {
                Ok(()) => {
                    log_method_once(LOG_FLAG_CLONE, "clone");
                    return Ok(());
                }
                Err(err) if is_reflink_fallback_error(&err) => {
                    state.fetch_max(LINK_STATE_HARDLINK, Ordering::Relaxed);
                }
                Err(err) => return Err(err),
            },
            LINK_STATE_HARDLINK => match fs::hard_link(source, target) {
                Ok(()) => {
                    log_method_once(LOG_FLAG_HARDLINK, "hardlink");
                    return Ok(());
                }
                Err(err) if is_hardlink_fallback_error(&err) => {
                    state.fetch_max(LINK_STATE_COPY, Ordering::Relaxed);
                }
                Err(err) => return Err(err),
            },
            _ => {
                log_method_once(LOG_FLAG_COPY, "copy");
                return fs::copy(source, target).map(drop);
            }
        }
    }
}

/// `CloneOrCopy`'s clone → copy chain with the same per-process cache
/// as [`auto_link`]. Differs from `Auto` by skipping the hardlink tier
/// entirely — matches pnpm's `createCloneOrCopyImporter`, which on
/// first reflink failure reassigns its closure directly to `copyPkg`.
/// Same error-narrowing as `auto_link`: only capability failures
/// downgrade; real errors propagate.
fn clone_or_copy_link(state: &AtomicU8, source: &Path, target: &Path) -> io::Result<()> {
    loop {
        match state.load(Ordering::Relaxed) {
            LINK_STATE_CLONE => match reflink_copy::reflink(source, target) {
                Ok(()) => {
                    log_method_once(LOG_FLAG_CLONE, "clone");
                    return Ok(());
                }
                Err(err) if is_reflink_fallback_error(&err) => {
                    state.fetch_max(LINK_STATE_COPY, Ordering::Relaxed);
                }
                Err(err) => return Err(err),
            },
            _ => {
                log_method_once(LOG_FLAG_COPY, "copy");
                return fs::copy(source, target).map(drop);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn write_source(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).expect("write source file");
        path
    }

    /// `Copy` always succeeds regardless of filesystem capabilities, so
    /// it's the safest method to assert against on CI.
    #[test]
    fn copy_materializes_the_file_contents() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"hello");
        let dst = tmp.path().join("nested/dst.txt");

        link_file(PackageImportMethod::Copy, &src, &dst).expect("link_file should succeed");

        assert_eq!(fs::read(&dst).unwrap(), b"hello");
        // A plain copy leaves the two files as independent inodes.
        let src_ino = fs::metadata(&src).unwrap();
        let dst_ino = fs::metadata(&dst).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_ne!(src_ino.ino(), dst_ino.ino());
        }
        #[cfg(not(unix))]
        let _ = (src_ino, dst_ino);
    }

    /// Hardlinking in the same directory on the same filesystem works on
    /// every mainstream OS the project supports. We verify the post-link
    /// inodes match (on unix) or that the contents match (otherwise).
    #[test]
    fn hardlink_shares_contents_with_source() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"shared");
        let dst = tmp.path().join("nested/dst.txt");

        link_file(PackageImportMethod::Hardlink, &src, &dst).expect("link_file should succeed");

        assert_eq!(fs::read(&dst).unwrap(), b"shared");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let src_meta = fs::metadata(&src).unwrap();
            let dst_meta = fs::metadata(&dst).unwrap();
            assert_eq!(src_meta.ino(), dst_meta.ino(), "hardlinked files share an inode");
            assert!(src_meta.nlink() >= 2, "hardlink should bump nlink");
        }
    }

    /// `Auto` must succeed on any filesystem because it falls through to
    /// `fs::copy`. We point it at a `tmpfs`-like temp dir — reflink and
    /// hardlink may or may not be available, but copy always is.
    #[test]
    fn auto_falls_through_to_a_working_method() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"auto");
        let dst = tmp.path().join("nested/dst.txt");

        link_file(PackageImportMethod::Auto, &src, &dst).expect("Auto should always succeed");
        assert_eq!(fs::read(&dst).unwrap(), b"auto");
    }

    /// If the target already exists, `link_file` is a no-op — it must not
    /// error (which `fs::hard_link` / `reflink` would do on their own) or
    /// overwrite the existing contents.
    #[test]
    fn existing_target_is_preserved() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"new");
        let dst = tmp.path().join("dst.txt");
        fs::write(&dst, b"old").unwrap();

        for method in [
            PackageImportMethod::Auto,
            PackageImportMethod::Copy,
            PackageImportMethod::Hardlink,
            PackageImportMethod::Clone,
            PackageImportMethod::CloneOrCopy,
        ] {
            link_file(method, &src, &dst).expect("existing target should short-circuit");
            assert_eq!(fs::read(&dst).unwrap(), b"old", "method {method:?} must not overwrite");
        }
    }

    /// Explicit `Hardlink` must surface non-`EXDEV` link-creation errors
    /// instead of silently falling back — matches pnpm's `linkOrCopy`,
    /// which only swallows `EXDEV` (and a couple of other kernel-level
    /// "not permitted" codes, not modelled here). We drive the error
    /// path by pointing at a non-existent source (`NotFound`, which is
    /// not `EXDEV`) so the failure is deterministic on every platform.
    #[test]
    fn explicit_hardlink_surfaces_errors() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst.txt");

        let err =
            link_file(PackageImportMethod::Hardlink, &src, &dst).expect_err("no source → error");
        assert!(matches!(err, LinkFileError::Import { .. }), "got: {err:?}");
    }

    /// `CloneOrCopy` has to succeed on any filesystem because
    /// `clone_or_copy_link` falls back to `fs::copy` when the reflink
    /// attempt fails with a capability error. This hits the match arm
    /// directly — the `existing_target_is_preserved` loop
    /// short-circuits before the arm ever runs, so without this we had
    /// no coverage of the real code path.
    #[test]
    fn clone_or_copy_materializes_the_file_contents() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"clone-or-copy");
        let dst = tmp.path().join("nested/dst.txt");

        link_file(PackageImportMethod::CloneOrCopy, &src, &dst)
            .expect("CloneOrCopy should always succeed");
        assert_eq!(fs::read(&dst).unwrap(), b"clone-or-copy");
    }

    /// Explicit `Clone` must propagate errors rather than silently
    /// copying. Pointing at a non-existent source gives us a
    /// deterministic failure on every FS regardless of reflink
    /// support, so the test doesn't need a btrfs / APFS runner.
    #[test]
    fn explicit_clone_surfaces_errors() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst.txt");

        let err = link_file(PackageImportMethod::Clone, &src, &dst).expect_err("no source → error");
        assert!(matches!(err, LinkFileError::Import { .. }), "got: {err:?}");
    }

    /// A dangling symlink left behind by an interrupted install is a
    /// corrupt target: if we short-circuit on it as "already present"
    /// the package ends up with a silently-missing file while the
    /// install reports success. Remove the broken link, re-materialize,
    /// and confirm the final dirent is a real file with the expected
    /// contents.
    #[test]
    #[cfg(unix)]
    fn dangling_symlink_is_replaced() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"fresh");
        let dst = tmp.path().join("dst.txt");
        std::os::unix::fs::symlink(tmp.path().join("never-created"), &dst).unwrap();

        link_file(PackageImportMethod::Hardlink, &src, &dst)
            .expect("dangling symlink should be scrubbed, then hardlinked");

        let meta = fs::symlink_metadata(&dst).unwrap();
        assert!(!meta.file_type().is_symlink(), "dangling link must be replaced with a real file");
        assert_eq!(fs::read(&dst).unwrap(), b"fresh");
    }

    /// Live symlinks (pointing at real files) should still short-circuit
    /// — they're legitimate user state, not corruption from an
    /// interrupted install. Observable: we don't remove the link, and
    /// we don't overwrite its target either.
    #[test]
    #[cfg(unix)]
    fn live_symlink_short_circuits() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"new");
        let real_target = write_source(tmp.path(), "existing.txt", b"old");
        let dst = tmp.path().join("dst.txt");
        std::os::unix::fs::symlink(&real_target, &dst).unwrap();

        link_file(PackageImportMethod::Hardlink, &src, &dst)
            .expect("live symlink should short-circuit");

        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&real_target).unwrap(), b"old", "target must not be overwritten");
    }

    /// A one-off `NotFound` / `PermissionDenied` / `AlreadyExists` on
    /// a single file must not downgrade the cache — those are
    /// per-call errors, not capability errors. A different source /
    /// target later in the install would still succeed at the current
    /// tier, and we'd have permanently disabled it for no reason.
    /// Pin the behaviour for `Auto`; the error propagates verbatim
    /// and the cache stays at `CLONE`.
    #[test]
    fn auto_call_errors_propagate_without_downgrading() {
        let state = AtomicU8::new(LINK_STATE_CLONE);
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst");

        let err = auto_link(&state, &src, &dst).expect_err("missing source → NotFound");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            state.load(Ordering::Relaxed),
            LINK_STATE_CLONE,
            "NotFound must not poison the cache",
        );
    }

    /// Once `Auto`'s state is `COPY`, we use `fs::copy` and must not
    /// re-attempt reflink / hardlink. Observable: a successful link
    /// with state pre-seeded to `COPY` has independent inodes (copy
    /// semantics), not shared ones (hardlink).
    #[test]
    #[cfg(unix)]
    fn auto_respects_cached_copy_state() {
        use std::os::unix::fs::MetadataExt;

        let state = AtomicU8::new(LINK_STATE_COPY);
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"cached-copy");
        let dst = tmp.path().join("dst.txt");

        auto_link(&state, &src, &dst).expect("copy should succeed");

        assert_eq!(fs::read(&dst).unwrap(), b"cached-copy");
        assert_ne!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino(),
            "state=COPY must not hardlink",
        );
        assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_COPY, "state must not drift");
    }

    /// State=HARDLINK means Auto skips the reflink attempt and jumps
    /// straight to `fs::hard_link`. Observable: shared inode on unix.
    #[test]
    #[cfg(unix)]
    fn auto_respects_cached_hardlink_state() {
        use std::os::unix::fs::MetadataExt;

        let state = AtomicU8::new(LINK_STATE_HARDLINK);
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"cached-hardlink");
        let dst = tmp.path().join("dst.txt");

        auto_link(&state, &src, &dst).expect("hardlink should succeed on same-FS tempdir");

        assert_eq!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino(),
            "state=HARDLINK must hardlink, not copy",
        );
        assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_HARDLINK, "state must not drift");
    }

    /// Same propagate-on-call-error property for `CloneOrCopy`.
    /// Missing source must not downgrade the cache.
    #[test]
    fn clone_or_copy_call_errors_propagate_without_downgrading() {
        let state = AtomicU8::new(LINK_STATE_CLONE);
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst");

        let err = clone_or_copy_link(&state, &src, &dst).expect_err("missing source → NotFound");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            state.load(Ordering::Relaxed),
            LINK_STATE_CLONE,
            "NotFound must not poison the cache",
        );
    }

    /// Pin the classification helper directly — the state-machine
    /// tests above exercise the common kinds (`NotFound` →
    /// propagate), but we also care that the capability codes we
    /// added ENOTTY and EOPNOTSUPP for actually trigger the fallback.
    #[test]
    fn is_reflink_fallback_error_classifies_capability_codes() {
        // NotFound must NOT be a capability error.
        let not_found = io::Error::from(io::ErrorKind::NotFound);
        assert!(!is_reflink_fallback_error(&not_found));

        // Unsupported IS a capability error.
        let unsupported = io::Error::from(io::ErrorKind::Unsupported);
        assert!(is_reflink_fallback_error(&unsupported));

        // ENOTTY (25) — what ext4 returns for ioctl_ficlone on a FS
        // without reflink support. This is the CI-critical case.
        let enotty = io::Error::from_raw_os_error(25);
        assert!(is_reflink_fallback_error(&enotty));

        // EXDEV (18) is cross-device — also a fallback trigger.
        let exdev = io::Error::from_raw_os_error(18);
        assert!(is_reflink_fallback_error(&exdev));
    }

    /// Pre-seed `CloneOrCopy` state to `COPY` and verify it uses
    /// `fs::copy` — mirrors `auto_respects_cached_copy_state`. Also
    /// confirms we skip the hardlink tier entirely (pnpm
    /// `createCloneOrCopyImporter` has no hardlink fallback).
    #[test]
    #[cfg(unix)]
    fn clone_or_copy_respects_cached_copy_state() {
        use std::os::unix::fs::MetadataExt;

        let state = AtomicU8::new(LINK_STATE_COPY);
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"cached");
        let dst = tmp.path().join("dst.txt");

        clone_or_copy_link(&state, &src, &dst).expect("copy should succeed");

        assert_ne!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino(),
            "state=COPY must not hardlink",
        );
        assert_eq!(state.load(Ordering::Relaxed), LINK_STATE_COPY, "state must not drift");
    }
}
