use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_npmrc::PackageImportMethod;
use std::{
    fs, io,
    path::{Path, PathBuf},
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

/// Materialize a CAFS file into `target_link` using `method`.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(
    method: PackageImportMethod,
    source_file: &Path,
    target_link: &Path,
) -> Result<(), LinkFileError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| LinkFileError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    // pnpm's documented Auto fallback chain: clone → hardlink → copy.
    // Each step broad-catches because "operation not supported" surfaces
    // as different `io::ErrorKind`s depending on platform and filesystem
    // (EOPNOTSUPP, EXDEV, EPERM, …) and pnpm itself doesn't try to
    // enumerate them.
    //
    // Hardlinking a file from the store into `node_modules` means any
    // package that edits its own files at runtime (postinstall scripts
    // are the usual offender) ends up mutating the shared store copy.
    // Pnpm guards against this by falling back to copy for packages that
    // declare a postinstall script; pacquet doesn't run postinstall
    // scripts yet, so there's nothing to gate on here — revisit when
    // script execution lands.
    let result = match method {
        PackageImportMethod::Auto => reflink_copy::reflink(source_file, target_link)
            .or_else(|_| fs::hard_link(source_file, target_link))
            .or_else(|_| fs::copy(source_file, target_link).map(drop)),
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

    /// Explicit `Hardlink` across devices (EXDEV) must surface the error
    /// instead of silently falling back — that's what `Auto` is for. We
    /// simulate a hard-link failure by pointing at a non-existent source.
    #[test]
    fn explicit_hardlink_surfaces_errors() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join("dst.txt");

        let err =
            link_file(PackageImportMethod::Hardlink, &src, &dst).expect_err("no source → error");
        assert!(matches!(err, LinkFileError::CreateLink { .. }), "got: {err:?}");
    }
}
