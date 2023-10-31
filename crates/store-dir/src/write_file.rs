use crate::{FileHash, FileSuffix, StoreDir, TarballIndex};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, EnsureFileError};
use sha2::{Digest, Sha512};
use ssri::Integrity;
use std::{fs, path::PathBuf};

/// Error type of [`StoreDir::write_non_index_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteNonIndexFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a file from an npm package to the store directory.
    pub fn write_non_index_file(
        &self,
        buffer: &[u8],
        suffix: Option<FileSuffix>,
    ) -> Result<(PathBuf, FileHash), WriteNonIndexFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.file_path_by_content_address(file_hash, suffix);

        ensure_file(&file_path, buffer).map_err(WriteNonIndexFileError::WriteFile)?;

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

/// Error type of [`StoreDir::write_tarball_index_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteTarballIndexFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a JSON file that indexes files in a tarball to the store directory.
    pub fn write_tarball_index_file(
        &self,
        tarball_integrity: &Integrity,
        index_content: &TarballIndex,
    ) -> Result<(), WriteTarballIndexFileError> {
        let file_path = self.tarball_index_file_path(tarball_integrity);
        let index_content =
            serde_json::to_string(&index_content).expect("convert a TarballIndex to JSON");
        ensure_file(&file_path, index_content.as_bytes())
            .map_err(WriteTarballIndexFileError::WriteFile)
    }
}
