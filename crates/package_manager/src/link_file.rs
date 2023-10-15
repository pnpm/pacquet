use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Error type for [`link_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LinkFileError {
    #[display("cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

/// Reflink or copy a single file.
///
/// * If `target_link` already exists, do nothing.
/// * If parent dir of `target_link` doesn't exist, it will be created.
pub fn link_file(source_file: &Path, target_link: &Path) -> Result<(), LinkFileError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| LinkFileError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
        LinkFileError::CreateLink {
            from: source_file.to_path_buf(),
            to: target_link.to_path_buf(),
            error,
        }
    })?; // TODO: add hardlink

    Ok(())
}
