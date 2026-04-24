use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Error type of [`ensure_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum EnsureFileError {
    #[display("Failed to create the parent directory at {parent_dir:?}: {error}")]
    CreateDir {
        parent_dir: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to create file at {file_path:?}: {error}")]
    CreateFile {
        file_path: PathBuf,
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

/// Ensure `dir` (and any missing ancestors) exists. Idempotent.
///
/// Split out from [`ensure_file`] so hot-path callers (the CAFS writer)
/// can cache which directories they've already created and skip the
/// syscall cost when they have — `fs::create_dir_all` does a `stat` on
/// every call even when the directory already exists, which adds up to
/// one wasted `stat` per file on a cold install.
pub fn ensure_parent_dir(dir: &Path) -> Result<(), EnsureFileError> {
    fs::create_dir_all(dir)
        .map_err(|error| EnsureFileError::CreateDir { parent_dir: dir.to_path_buf(), error })
}

/// Write `content` to `file_path` unless it already exists.
///
/// **The parent directory must already exist.** Callers that can't
/// guarantee that should call [`ensure_parent_dir`] first — splitting
/// the two lets the CAFS writer share one `create_dir_all` per shard
/// instead of paying it per file.
pub fn ensure_file(
    file_path: &Path,
    content: &[u8],
    #[cfg_attr(windows, allow(unused))] mode: Option<u32>,
) -> Result<(), EnsureFileError> {
    if file_path.exists() {
        return Ok(());
    }

    let mut options = OpenOptions::new();
    options.write(true).create(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(mode) = mode {
            options.mode(mode);
        }
    }

    options
        .open(file_path)
        .map_err(|error| EnsureFileError::CreateFile { file_path: file_path.to_path_buf(), error })?
        .write_all(content)
        .map_err(|error| EnsureFileError::WriteFile { file_path: file_path.to_path_buf(), error })
}
