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

/// Read up to `buf.len()` bytes of `path` starting at byte `offset`.
/// **Single underlying syscall** — POSIX `read(2)` is allowed to
/// return fewer bytes than requested (a "short read"), so callers
/// that need a fully-filled buffer must loop. Use
/// [`crate::read_head_filled`] for that — it stays generic over this
/// trait so test fakes don't have to grow.
///
/// Used by [`crate::search_script_runtime`] (via [`crate::read_head_filled`])
/// to detect the script runtime via the shebang at the head of a bin
/// file.
pub trait FsReadHead {
    fn read_head(path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
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

/// List the entries of a directory.
///
/// Returns an `impl Iterator<Item = PathBuf>` rather than a
/// `Vec<PathBuf>` so the production impl can stream entries straight
/// out of `fs::ReadDir` without materialising the whole list, and so
/// fakes don't have to declare an associated `Iter` type per impl —
/// they just return whatever concrete iterator they want.
///
/// We deliberately don't expose `fs::ReadDir` directly: its iterator
/// type is platform-specific and yields `io::Result<DirEntry>`,
/// which would force every fake to fabricate a `DirEntry` (and tie
/// the trait to libstd's filesystem types). Yielding plain
/// `PathBuf` keeps fakes trivial.
pub trait FsReadDir {
    fn read_dir(path: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>>;
}

/// Recursively walk `path` and yield every regular file found beneath
/// it (depth-first, no symlink follow). Used by
/// [`crate::get_bins_from_package_manifest`] to enumerate
/// `directories.bin` entries.
///
/// Returns an `impl Iterator<Item = PathBuf>` rather than a
/// `Vec<PathBuf>` so the production walker streams entries straight
/// out of `walkdir` instead of materialising the whole list up front.
/// `directories.bin` trees are usually tiny, but the abstraction
/// shouldn't bake in an allocation the real implementation doesn't
/// need. Fakes return whatever concrete iterator they want — typically
/// [`std::iter::empty`] for the unreachable-walk case or
/// [`Vec::into_iter`] when feeding a fixed list of paths.
///
/// `walkdir`'s builder exposes many knobs (`follow_links`, `min_depth`,
/// `max_depth`, `sort_by`, …); pacquet uses just one (`follow_links =
/// false`). Mirroring the full builder through the trait would be
/// over-engineering for the single call site, so the trait keeps its
/// surface dead-simple and the impl bakes the option in. If a future
/// caller needs different walk options, add a new capability rather
/// than parameterise this one.
pub trait FsWalkFiles {
    fn walk_files(path: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>>;
}

/// Create a directory and any missing ancestors. Used to prepare
/// `<modules_dir>/.bin` and per-slot `node_modules/.bin` directories.
pub trait FsCreateDirAll {
    fn create_dir_all(path: &Path) -> io::Result<()>;
}

/// Write `bytes` to `path`, replacing the file's contents if it
/// exists. Used to write the three shim flavors (`.sh`, `.cmd`,
/// `.ps1`).
///
/// **Not atomic.** This trait promises only what `std::fs::write`
/// promises: a single `write(2)` call, no tempfile-rename guard.
/// A SIGINT mid-write can leave a truncated file. If a future
/// caller needs atomic write semantics, build it on top of this
/// trait (write to a sibling tempfile, then rename) rather than
/// hiding the algorithm inside the capability — keeping the trait
/// minimal lets every callsite see exactly what guarantees it
/// inherits.
pub trait FsWrite {
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
    fn read_head(path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))?;
        }
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
    fn read_dir(path: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
        // `flatten()` silently drops per-entry errors (matches the
        // prior collect-then-flatten shape and the `tinyglobby`-style
        // ENOENT-on-subtree behaviour pacquet's callers expect).
        Ok(std::fs::read_dir(path)?.flatten().map(|entry| entry.path()))
    }
}

impl FsWalkFiles for RealApi {
    fn walk_files(path: &Path) -> io::Result<impl Iterator<Item = std::path::PathBuf>> {
        // `flatten()` silently drops per-entry errors (matches pnpm's
        // tinyglobby ENOENT-on-subtree behaviour); the top-level
        // missing-dir case also flows through here as a single
        // dropped `Err`, so a missing `bin_dir` produces an empty
        // stream rather than an error.
        Ok(walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf()))
    }
}

impl FsCreateDirAll for RealApi {
    fn create_dir_all(path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }
}

impl FsWrite for RealApi {
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
