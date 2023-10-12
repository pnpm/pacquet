use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::symlink_dir;
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Error type for [`symlink_package`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum SymlinkPackageError {
    #[display(fmt = "Failed to create directory at {dir:?}: {error}")]
    CreateParentDir {
        dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },

    #[display(fmt = "Failed to create symlink at {symlink_path:?} to {symlink_target:?}: {error}")]
    SymlinkDir {
        symlink_target: PathBuf,
        symlink_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Create symlink for a package.
///
/// * If ancestors of `symlink_path` don't exist, they will be created recursively.
/// * If `symlink_path` already exists, skip.
/// * If `symlink_path` doesn't exist, a symlink pointing to `symlink_target` will be created.
pub fn symlink_package(
    symlink_target: &Path,
    symlink_path: &Path,
) -> Result<(), SymlinkPackageError> {
    // NOTE: symlink target in pacquet is absolute yet in pnpm is relative
    // TODO: change symlink target to relative
    if let Some(parent) = symlink_path.parent() {
        fs::create_dir_all(parent).map_err(|error| SymlinkPackageError::CreateParentDir {
            dir: parent.to_path_buf(),
            error,
        })?;
    }
    if let Err(error) = symlink_dir(symlink_target, symlink_path) {
        match error.kind() {
            ErrorKind::AlreadyExists => {}
            _ => {
                return Err(SymlinkPackageError::SymlinkDir {
                    symlink_target: symlink_target.to_path_buf(),
                    symlink_path: symlink_path.to_path_buf(),
                    error,
                })
            }
        }
    }
    Ok(())
}
