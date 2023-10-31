use std::{fs, io, path::Path};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return junction::create(original, link); // junctions instead of symlinks because symlinks may require elevated privileges.
}

/// Write `content` to `file_path` unless it already exists.
///
/// Ancestor directories will be created if they don't already exist.
///
/// **TODO:** separate 2 error cases and add more details
pub fn ensure_file(file_path: &Path, content: &[u8]) -> io::Result<()> {
    if file_path.exists() {
        return Ok(());
    }

    let parent_dir = file_path.parent().unwrap();
    fs::create_dir_all(parent_dir)?;
    fs::write(file_path, content)
}
