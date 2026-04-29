//! Per-capability dependency-injection traits and the production
//! [`RealApi`] provider. Mirrors the pattern documented at
//! <https://github.com/pnpm/pacquet/pull/332#issuecomment-4345054524>:
//!
//! 1. One trait per capability.
//! 2. Functions bind only what they consume (compose bounds).
//! 3. No `&self` on capability methods.
//! 4. Production callers turbofish the real impl explicitly.
//!
//! Tests inject unit-struct fakes to exercise IO error paths that the
//! real filesystem can't reach portably (e.g. permission denied,
//! ENOSPC).

use std::{io, path::Path};

/// Read up to a buffer of bytes from a file path. Used by
/// [`crate::search_script_runtime`] to detect the script runtime via
/// the shebang.
pub trait FsReadHead {
    fn read_head(path: &Path, buf: &mut [u8]) -> io::Result<usize>;
}

/// Read the entire contents of a file into a `Vec<u8>`. Used to read
/// `package.json` files when collecting bin sources.
pub trait FsReadFile {
    fn read_file(path: &Path) -> io::Result<Vec<u8>>;
}

/// Read the entire contents of a file into a `String`. Used by
/// [`crate::link_bins_of_packages`] to short-circuit on warm reinstalls
/// where the existing shim already targets the same bin file.
pub trait FsReadString {
    fn read_to_string(path: &Path) -> io::Result<String>;
}

/// List the entries of a directory. We collect into a `Vec<PathBuf>`
/// rather than expose `fs::ReadDir` so test fakes don't need to fabricate
/// platform-specific iterator types. Eager collection is fine — the
/// only call sites are `<modules_dir>/` and per-slot `node_modules/`,
/// both of which are small.
pub trait FsReadDir {
    fn read_dir(path: &Path) -> io::Result<Vec<std::path::PathBuf>>;
}

/// Create a directory and any missing ancestors. Used to prepare
/// `<modules_dir>/.bin` and per-slot `node_modules/.bin` directories.
pub trait FsCreateDirAll {
    fn create_dir_all(path: &Path) -> io::Result<()>;
}

/// Write `bytes` to `path`, replacing the file's contents if it
/// exists. Used to write the three shim flavors (`.sh`, `.cmd`,
/// `.ps1`).
pub trait FsWriteAtomic {
    fn write(path: &Path, bytes: &[u8]) -> io::Result<()>;
}

/// Apply a unix mode (or no-op on Windows) to a path. Used to chmod
/// shims and target binaries to `0o755` for executable-bit parity with
/// pnpm. The mode argument is `u32` so callers can compute it (read
/// metadata, OR in `0o111`, write back).
pub trait FsSetPermissions {
    fn set_executable(path: &Path) -> io::Result<()>;
    fn ensure_executable_bits(path: &Path) -> io::Result<()>;
}

/// The production filesystem provider — every method delegates straight
/// to `std::fs`.
pub struct RealApi;

impl FsReadHead for RealApi {
    fn read_head(path: &Path, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        file.read(buf)
    }
}

impl FsReadFile for RealApi {
    fn read_file(path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

impl FsReadString for RealApi {
    fn read_to_string(path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

impl FsReadDir for RealApi {
    fn read_dir(path: &Path) -> io::Result<Vec<std::path::PathBuf>> {
        Ok(std::fs::read_dir(path)?.flatten().map(|entry| entry.path()).collect())
    }
}

impl FsCreateDirAll for RealApi {
    fn create_dir_all(path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }
}

impl FsWriteAtomic for RealApi {
    fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        std::fs::write(path, bytes)
    }
}

impl FsSetPermissions for RealApi {
    fn set_executable(path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }

    fn ensure_executable_bits(path: &Path) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(path)?;
            let mode = metadata.permissions().mode() | 0o111;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(())
        }
    }
}
