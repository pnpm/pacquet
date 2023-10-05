use pacquet_fs::symlink_dir;
use std::{fs, io::ErrorKind, path::Path};

/// Create symlink for a package.
///
/// * If ancestors of `symlink_path` don't exist, they will be created recursively.
/// * If `symlink_path` already exists, skip.
/// * If `symlink_path` doesn't exist, a symlink pointing to `symlink_target` will be created.
pub fn symlink_pkg(symlink_target: &Path, symlink_path: &Path) {
    // NOTE: symlink target in pacquet is absolute yet in pnpm is relative
    // TODO: change symlink target to relative
    if let Some(parent) = symlink_path.parent() {
        fs::create_dir_all(parent).expect("make sure node_modules exist"); // TODO: proper error propagation
    }
    if let Err(error) = symlink_dir(symlink_target, symlink_path) {
        match error.kind() {
            ErrorKind::AlreadyExists => {}
            _ => panic!(
                "Failed to create symlink at {symlink_path:?} to {symlink_target:?}: {error}"
            ), // TODO: proper error propagation
        }
    }
}
