//! Path-manipulation helpers shared across [`crate::bin_resolver`] and
//! [`crate::shim`]. Kept private to the crate.

use std::path::{Component, Path, PathBuf};

/// Lexically resolve `.` and `..` in `path` without touching the filesystem.
///
/// `.` (CurDir) components are dropped. `..` (ParentDir) components pop the
/// previous component when one exists, otherwise they pass through (so a
/// leading `..` survives). All other components are passed through verbatim,
/// preserving root and prefix entries on platforms that have them.
///
/// Filesystem-free is the whole point: callers in
/// [`crate::bin_resolver::is_subdir`] and [`crate::shim::relative_path_from`]
/// run the check before the target files exist on disk, where
/// `std::fs::canonicalize` cannot help. Mirrors pnpm's `is-subdir`, which is
/// also purely lexical.
pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}
