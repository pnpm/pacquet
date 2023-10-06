use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Display, Error, Diagnostic)]
pub enum AutoImportError {
    #[display(fmt = "cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display(fmt = "fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[error(source)]
        error: io::Error,
    },
}

pub fn auto_import(source_file: &Path, target_link: &Path) -> Result<(), AutoImportError> {
    if target_link.exists() {
        return Ok(());
    }

    if let Some(parent_dir) = target_link.parent() {
        fs::create_dir_all(parent_dir).map_err(|error| AutoImportError::CreateDir {
            dirname: parent_dir.to_path_buf(),
            error,
        })?;
    }

    reflink_copy::reflink_or_copy(source_file, target_link).map_err(|error| {
        AutoImportError::CreateLink {
            from: source_file.to_path_buf(),
            to: target_link.to_path_buf(),
            error,
        }
    })?; // TODO: add hardlink

    Ok(())
}
