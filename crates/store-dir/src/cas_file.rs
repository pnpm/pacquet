use crate::{FileHash, StoreDir};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, make_file_executable, EnsureFileError};
use sha2::{Digest, Sha512};
use std::path::PathBuf;

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
        executable: bool,
    ) -> Result<(PathBuf, FileHash), WriteCasFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.cas_file_path(file_hash, executable);

        ensure_file(&file_path, buffer).map_err(WriteCasFileError::WriteFile)?;
        make_file_executable(&file_path).expect("make the file executable"); // TODO: propagate this error

        Ok((file_path, file_hash))
    }
}
