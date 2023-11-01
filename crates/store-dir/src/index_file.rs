use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, EnsureFileError};
use serde::{Deserialize, Serialize};
use ssri::{Algorithm, Integrity};
use std::{collections::HashMap, path::PathBuf};

impl StoreDir {
    /// Path to an index file of a tarball.
    pub fn tarball_index_file_path(&self, tarball_integrity: &Integrity) -> PathBuf {
        let (algorithm, hex) = tarball_integrity.to_hex();
        assert_eq!(algorithm, Algorithm::Sha512, "Only Sha512 is supported"); // TODO: propagate this error
        self.file_path_by_hex_str(&hex, "-index.json")
    }
}

/// Content of an index file (`$STORE_DIR/v3/files/*/*-index.json`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilesIndex {
    pub files: HashMap<String, TarballIndexFileAttrs>,
}

/// Value of the [`files`](PackageFilesIndex::files) map.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TarballIndexFileAttrs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u128>,
    pub integrity: String,
    pub mode: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
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
        index_content: &PackageFilesIndex,
    ) -> Result<(), WriteTarballIndexFileError> {
        let file_path = self.tarball_index_file_path(tarball_integrity);
        let index_content =
            serde_json::to_string(&index_content).expect("convert a TarballIndex to JSON");
        ensure_file(&file_path, index_content.as_bytes(), Some(0o666))
            .map_err(WriteTarballIndexFileError::WriteFile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssri::IntegrityOpts;

    #[test]
    fn tarball_index_file_path() {
        let store_dir = StoreDir::new("STORE_DIR");
        let tarball_integrity =
            IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(b"TARBALL CONTENT").result();
        let received = store_dir.tarball_index_file_path(&tarball_integrity);
        let expected = "STORE_DIR/v3/files/bc/d60799116ebef60071b9f2c7dafd7e2a4e1b366e341f750b2de52dd6995ab409b530f31b2b0a56c168a808a977156c3f5f13b026fb117d36314d8077f8733f-index.json";
        let expected: PathBuf = expected.split('/').collect();
        assert_eq!(&received, &expected);
    }
}
