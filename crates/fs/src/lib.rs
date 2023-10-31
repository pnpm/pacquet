use std::{io, path::Path};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return junction::create(original, link); // junctions instead of symlinks because symlinks may require elevated privileges.
}

/// Set file mode to 777 on POSIX platforms such as Linux or macOS,
/// or do nothing on Windows.
#[cfg_attr(windows, allow(unused))]
pub fn make_file_executable(file_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return {
        use std::{
            fs::{set_permissions, Permissions},
            os::unix::fs::PermissionsExt,
        };
        let permissions = Permissions::from_mode(0o777);
        set_permissions(file_path, permissions)
    };

    #[cfg(windows)]
    return Ok(());
}
