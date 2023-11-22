use std::io;

/// Bit mask to filter executable bits (`--x--x--x`).
pub const EXEC_MASK: u32 = 0b001_001_001;

/// All can read and execute, but only owner can write (`rwxr-xr-x`).
pub const EXEC_MODE: u32 = 0b111_101_101;

/// Whether a file mode has all executable bits.
pub fn is_all_exec(mode: u32) -> bool {
    mode & EXEC_MASK == EXEC_MASK
}

/// Set file mode to 777 on POSIX platforms such as Linux or macOS,
/// or do nothing on Windows.
#[cfg_attr(windows, allow(unused))]
pub fn make_file_executable(file: &std::fs::File) -> io::Result<()> {
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
