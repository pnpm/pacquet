use crate::{FileHash, FileSuffix, StoreDir};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, EnsureFileError};
use sha2::{Digest, Sha512};
use std::{fs, path::PathBuf};

/// Error type of [`StoreDir::write_cas_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteCasFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a file from an npm package to the store directory.
    pub fn write_cas_file(
        &self,
        buffer: &[u8],
        suffix: Option<FileSuffix>,
    ) -> Result<(PathBuf, FileHash), WriteCasFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.file_path_by_content_address(file_hash, suffix);

        ensure_file(&file_path, buffer).map_err(WriteCasFileError::WriteFile)?;

        #[cfg(unix)]
        {
            use std::{fs::Permissions, os::unix::fs::PermissionsExt};
            if suffix == Some(FileSuffix::Exec) {
                let permissions = Permissions::from_mode(0o777);
                fs::set_permissions(&file_path, permissions).expect("make the file executable");
            }
        }

        Ok((file_path, file_hash))
    }
}
