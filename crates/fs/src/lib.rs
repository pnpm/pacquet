use std::{fs::File, io, path::Path};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return junction::create(original, link); // junctions instead of symlinks because symlinks may require elevated privileges.
}

/// Executable bit mask for a UNIX file permission
#[cfg(unix)]
pub const EXEC_MASK: u32 = 0b001001001; // --x--x--x

/// Set file mode to 777 on POSIX platforms such as Linux or macOS,
/// or do nothing on Windows.
#[cfg_attr(windows, allow(unused))]
pub fn make_file_executable(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    return {
        use std::{
            fs::Permissions,
            os::unix::fs::{MetadataExt, PermissionsExt},
        };
        let mode = file.metadata()?.mode();
        let mode = mode | EXEC_MASK;
        let permissions = Permissions::from_mode(mode);
        file.set_permissions(permissions)
    };

    #[cfg(windows)]
    return Ok(());
}

/// Set file mode to 777 on POSIX platforms such as Linux or macOS,
/// or do nothing on Windows.
#[cfg_attr(windows, allow(unused))]
pub fn make_path_executable(file_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return File::open(file_path).and_then(|file| make_file_executable(&file));

    #[cfg(windows)]
    return Ok(());
}
