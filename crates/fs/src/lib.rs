use std::{io, os, path::Path};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return os::windows::fs::symlink_dir(original, link);
}
