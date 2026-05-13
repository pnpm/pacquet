use std::{
    io,
    path::{Path, PathBuf},
};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return junction::create(original, link); // junctions instead of symlinks because symlinks may require elevated privileges.
}

/// Remove a symlink (or junction on Windows) previously created with
/// [`symlink_dir`].
///
/// On Unix a directory symlink is a file-shaped entry and removed
/// with `fs::remove_file`. On Windows [`symlink_dir`] creates a
/// junction (a directory-shaped reparse point) so it needs
/// `fs::remove_dir` to be unlinked — `remove_file` returns
/// `ERROR_ACCESS_DENIED`. Wrapping both in one helper keeps the
/// platform split contained.
pub fn remove_symlink_dir(link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::fs::remove_file(link);
    #[cfg(windows)]
    return std::fs::remove_dir(link);
}

/// Read the target of a directory symlink (or junction on Windows).
///
/// On Unix this is just [`std::fs::read_link`]. On Windows the
/// stdlib's `read_link` only handles
/// [`IO_REPARSE_TAG_SYMLINK`](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-point-tags)
/// reparse points and returns `ERROR_NOT_A_REPARSE_POINT`
/// (`InvalidInput`) for `IO_REPARSE_TAG_MOUNT_POINT` junctions —
/// see [`rust-lang/rust#28528`](https://github.com/rust-lang/rust/issues/28528),
/// which has been open since 2015. Since [`symlink_dir`] creates
/// junctions on Windows, every entry pacquet writes would
/// otherwise be unreadable. Fall back to `junction::get_target`
/// on `InvalidInput` to handle the junction case while keeping
/// `fs::read_link` as the fast path for true symlinks. (Plain
/// backticks rather than an intra-doc link because the `junction`
/// crate is only in scope on Windows targets — a link would
/// break the Linux doc build.)
pub fn read_symlink_dir(link: &Path) -> io::Result<PathBuf> {
    #[cfg(unix)]
    return std::fs::read_link(link);
    #[cfg(windows)]
    {
        match std::fs::read_link(link) {
            Ok(target) => Ok(target),
            // EINVAL on Windows from `read_link` means the reparse
            // point isn't a symbolic link tag — almost certainly a
            // junction, the only other kind of reparse point
            // pacquet's writer produces.
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => junction::get_target(link),
            Err(error) => Err(error),
        }
    }
}
