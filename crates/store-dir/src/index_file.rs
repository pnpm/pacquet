use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, EnsureFileError};
use serde::{Deserialize, Serialize};
use ssri::{Algorithm, Integrity};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

impl StoreDir {
    /// Path to an index file of a tarball.
    pub fn index_file_path(&self, tarball_integrity: &Integrity) -> PathBuf {
        let (algorithm, hex) = tarball_integrity.to_hex();
        assert!(
            matches!(algorithm, Algorithm::Sha512 | Algorithm::Sha1),
            "Only Sha1 and Sha512 are supported. {algorithm} isn't",
        ); // TODO: propagate this error
        self.file_path_by_hex_str(&hex, "-index.json")
    }
}

/// Content of an index file (`$STORE_DIR/v3/files/*/*-index.json`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilesIndex {
    pub files: HashMap<String, PackageFileInfo>,
}

/// Value of the [`files`](PackageFilesIndex::files) map.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFileInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u128>,
    pub integrity: String,
    pub mode: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Error type of [`StoreDir::write_index_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteIndexFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a JSON file that indexes files in a tarball to the store directory.
    pub fn write_index_file(
        &self,
        integrity: &Integrity,
        index_content: &PackageFilesIndex,
    ) -> Result<(), WriteIndexFileError> {
        let file_path = self.index_file_path(integrity);
        let index_content =
            serde_json::to_string(&index_content).expect("convert a TarballIndex to JSON");
        ensure_file(&file_path, index_content.as_bytes(), Some(0o666))
            .map_err(WriteIndexFileError::WriteFile)
    }
}

/// Error type of [`StoreDir::read_index_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum ReadIndexFileError {
    #[display("Failed to open {file_path:?}: {error}")]
    OpenFile {
        file_path: PathBuf,
        #[error(source)]
        error: io::Error,
    },
    #[display("Failed to parse content of {file_path:?}: {error}")]
    ParseFile {
        file_path: PathBuf,
        #[error(source)]
        error: serde_json::Error,
    },
}

impl StoreDir {
    /// Read an index file from the store directory.
    pub fn read_index_file(
        &self,
        integrity: &Integrity,
    ) -> Result<PackageFilesIndex, ReadIndexFileError> {
        let file_path = self.index_file_path(integrity);
        let file = match File::open(&file_path) {
            Ok(file) => file,
            Err(error) => return Err(ReadIndexFileError::OpenFile { file_path, error }),
        };
        match serde_json::from_reader(file) {
            Ok(content) => Ok(content),
            Err(error) => Err(ReadIndexFileError::ParseFile { file_path, error }),
        }
    }
}

impl ReadIndexFileError {
    pub fn file_path(&self) -> &Path {
        match self {
            ReadIndexFileError::OpenFile { file_path, .. } => file_path,
            ReadIndexFileError::ParseFile { file_path, .. } => file_path,
        }
    }

    pub fn io_error_kind(&self) -> Option<ErrorKind> {
        match self {
            ReadIndexFileError::OpenFile { error, .. } => Some(error.kind()),
            ReadIndexFileError::ParseFile { error, .. } => error.io_error_kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssri::IntegrityOpts;

    #[test]
    fn index_file_path() {
        let store_dir = StoreDir::new("STORE_DIR");
        let integrity =
            IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(b"TARBALL CONTENT").result();
        let received = store_dir.index_file_path(&integrity);
        let expected = "STORE_DIR/v3/files/bc/d60799116ebef60071b9f2c7dafd7e2a4e1b366e341f750b2de52dd6995ab409b530f31b2b0a56c168a808a977156c3f5f13b026fb117d36314d8077f8733f-index.json";
        let expected: PathBuf = expected.split('/').collect();
        assert_eq!(&received, &expected);
    }
}
