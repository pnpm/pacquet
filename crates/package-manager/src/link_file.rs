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
    #[display("fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

// Cached downgrade state for `PackageImportMethod::Auto`.
//
// Whether reflink / hardlink work is a property of the (source fs,
// target fs) pair, not of individual files. Once we observe a tier
// failing we stop trying it for the rest of the process — otherwise
// an install of a package with hundreds of files on a non-reflink FS
// would pay the "try reflink, fail" cost hundreds of times.
//
// The state is monotonic (`CLONE` → `HARDLINK` → `COPY`) and updated
// with `fetch_max`, so concurrent rayon workers racing on the first
// failure all converge to the same downgraded value without a lock.
// Worst case cost on startup is `N` stale attempts per tier where `N`
// is the rayon thread count — bounded, not per-file.
const AUTO_STATE_CLONE: u8 = 0;
const AUTO_STATE_HARDLINK: u8 = 1;
const AUTO_STATE_COPY: u8 = 2;

/// Materialize a CAFS file into `target_link` using `method`.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    method: PackageImportMethod,
    source_file: &Path,
    target_link: &Path,
) -> Result<(), LinkFileError> {
    // `exists()` follows symlinks, so a dangling symlink at
    // `target_link` (left behind by an interrupted prior install) would
    // slip through here and make the subsequent `hard_link` / `reflink`
    // fail with `AlreadyExists`, contradicting the "already exists → no
    // op" contract in the doc comment above. `symlink_metadata` asks
    // about the directory entry itself without following, which covers
    // files, directories, and symlinks (broken or otherwise).
    if fs::symlink_metadata(target_link).is_ok() {
        return Ok(());
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
    // Pnpm guards against this by falling back to copy for packages that
    // declare a postinstall script; pacquet doesn't run postinstall
    // scripts yet, so there's nothing to gate on here — revisit when
    // script execution lands.
    let result = match method {
        PackageImportMethod::Auto => {
            static AUTO_STATE: AtomicU8 = AtomicU8::new(AUTO_STATE_CLONE);
            auto_link(&AUTO_STATE, source_file, target_link)
        }
        PackageImportMethod::Hardlink => fs::hard_link(source_file, target_link),
        PackageImportMethod::Clone => reflink_copy::reflink(source_file, target_link),
        PackageImportMethod::CloneOrCopy => {
            reflink_copy::reflink_or_copy(source_file, target_link).map(drop)
        }
        PackageImportMethod::Copy => fs::copy(source_file, target_link).map(drop),
    };

    result.map_err(|error| LinkFileError::CreateLink {
        from: source_file.to_path_buf(),
        to: target_link.to_path_buf(),
        error,
    })
}

/// Auto's clone → hardlink → copy chain, using `state` to skip tiers
/// that have already failed in this process. Factored out so tests can
/// pass their own `AtomicU8` and exercise the downgrade logic in
/// isolation — the production path uses a `static` declared inside
/// [`link_file`]. Broad-catches each tier's errors because "operation
/// not supported" surfaces as different `io::ErrorKind`s depending on
/// platform and filesystem (`EOPNOTSUPP`, `EXDEV`, `EPERM`, …) and pnpm
/// itself doesn't try to enumerate them.
fn auto_link(state: &AtomicU8, source: &Path, target: &Path) -> io::Result<()> {
    loop {
        match state.load(Ordering::Relaxed) {
            AUTO_STATE_CLONE => match reflink_copy::reflink(source, target) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    state.fetch_max(AUTO_STATE_HARDLINK, Ordering::Relaxed);
                }
            },
            AUTO_STATE_HARDLINK => match fs::hard_link(source, target) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    state.fetch_max(AUTO_STATE_COPY, Ordering::Relaxed);
                }
            },
            _ => return fs::copy(source, target).map(drop),
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

    /// Explicit `Hardlink` must surface link-creation errors instead of
    /// silently falling back — that's what `Auto` is for. We drive the
    /// error path by pointing at a non-existent source (`NotFound`) so
    /// the failure is deterministic on every platform / filesystem.
    #[test]
    fn explicit_hardlink_surfaces_errors() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst.txt");

        let err =
            link_file(PackageImportMethod::Hardlink, &src, &dst).expect_err("no source → error");
        assert!(matches!(err, LinkFileError::CreateLink { .. }), "got: {err:?}");
    }

    /// `CloneOrCopy` has to succeed on any filesystem because
    /// `reflink_or_copy` falls back to a plain copy when the kernel
    /// can't reflink. This hits the match arm directly — the
    /// `existing_target_is_preserved` loop short-circuits before the
    /// arm ever runs, so without this we had no coverage of the real
    /// code path.
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

        let err =
            link_file(PackageImportMethod::Clone, &src, &dst).expect_err("no source → error");
        assert!(matches!(err, LinkFileError::CreateLink { .. }), "got: {err:?}");
    }

    /// A dangling symlink left behind by an interrupted install used
    /// to sneak past the `target_link.exists()` check (which follows
    /// symlinks) and then collide with `hard_link` / `reflink` as
    /// `AlreadyExists`. The doc comment promises "if target_link
    /// already exists, do nothing" — so the dangling link must be
    /// treated as already-present.
    #[test]
    #[cfg(unix)]
    fn dangling_symlink_is_treated_as_already_present() {
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"fresh");
        let dst = tmp.path().join("dst.txt");
        // Target of the symlink does not exist — the link is dangling.
        std::os::unix::fs::symlink(tmp.path().join("never-created"), &dst).unwrap();

        link_file(PackageImportMethod::Hardlink, &src, &dst)
            .expect("dangling symlink should short-circuit, not fail");
        // The dangling symlink should still be there, unchanged — we
        // don't attempt to replace it.
        assert!(fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
    }

    /// Core caching property: once Auto observes reflink failing, the
    /// state downgrades and subsequent calls skip reflink entirely.
    /// Using a non-existent source to force both reflink and hardlink
    /// to fail deterministically on every platform — we just want to
    /// drive the state machine to its terminal `COPY` state.
    #[test]
    fn auto_state_downgrades_monotonically_on_failure() {
        let state = AtomicU8::new(AUTO_STATE_CLONE);
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst");

        // First call: reflink fails → downgrade to HARDLINK; hardlink
        // fails → downgrade to COPY; copy fails → error bubbles up.
        // Final state = COPY.
        let _ = auto_link(&state, &src, &dst);
        assert_eq!(state.load(Ordering::Relaxed), AUTO_STATE_COPY);
    }

    /// Once state is `COPY`, Auto must use `fs::copy` and must not
    /// re-attempt reflink / hardlink. We can't observe the negative
    /// directly, but using a cross-file-type source that only `fs::copy`
    /// handles cleanly would be platform-sensitive — instead, assert on
    /// the observable: a successful link with a fresh state=COPY has
    /// independent inodes (copy semantics), not shared ones (hardlink).
    #[test]
    #[cfg(unix)]
    fn auto_respects_cached_copy_state() {
        use std::os::unix::fs::MetadataExt;

        let state = AtomicU8::new(AUTO_STATE_COPY);
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
        assert_eq!(state.load(Ordering::Relaxed), AUTO_STATE_COPY, "state must not drift");
    }

    /// State=HARDLINK means Auto skips the reflink attempt and jumps
    /// straight to `fs::hard_link`. Observable: shared inode on unix.
    #[test]
    #[cfg(unix)]
    fn auto_respects_cached_hardlink_state() {
        use std::os::unix::fs::MetadataExt;

        let state = AtomicU8::new(AUTO_STATE_HARDLINK);
        let tmp = tempdir().unwrap();
        let src = write_source(tmp.path(), "src.txt", b"cached-hardlink");
        let dst = tmp.path().join("dst.txt");

        auto_link(&state, &src, &dst).expect("hardlink should succeed on same-FS tempdir");

        assert_eq!(
            fs::metadata(&src).unwrap().ino(),
            fs::metadata(&dst).unwrap().ino(),
            "state=HARDLINK must hardlink, not copy",
        );
        assert_eq!(state.load(Ordering::Relaxed), AUTO_STATE_HARDLINK, "state must not drift");
    }
}
