use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Create a symlink to a directory.
///
/// The `link` path will be a symbolic link pointing to `original`.
pub fn symlink_dir(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(original, link);
    #[cfg(windows)]
    return junction::create(original, link); // junctions instead of symlinks because symlinks may require elevated privileges.
}

/// Error type of [`ensure_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum EnsureFileError {
    #[display("Failed to create the parent directory at {parent_dir:?}: {error}")]
    CreateDir {
        parent_dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to write to file at {file_path:?}: {error}")]
    WriteFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Write `content` to `file_path` unless it already exists.
///
/// Ancestor directories will be created if they don't already exist.
pub fn ensure_file(file_path: &Path, content: &[u8]) -> Result<(), EnsureFileError> {
    if file_path.exists() {
        return Ok(());
    }

    let parent_dir = file_path.parent().unwrap();
    fs::create_dir_all(parent_dir).map_err(|error| EnsureFileError::CreateDir {
        parent_dir: parent_dir.to_path_buf(),
        error,
    })?;
    fs::write(file_path, content)
        .map_err(|error| EnsureFileError::WriteFile { file_path: file_path.to_path_buf(), error })
}
