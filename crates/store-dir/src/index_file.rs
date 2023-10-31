use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{ensure_file, EnsureFileError};
use serde::{Deserialize, Serialize};
use ssri::Integrity;
use std::collections::HashMap;

/// Content of an index file (`$STORE_DIR/v3/files/*/*-index.json`).
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TarballIndex {
    pub files: HashMap<String, TarballIndexFileAttrs>,
}

/// Value of the [`files`](TarballIndex::files) map.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TarballIndexFileAttrs {
    // pub checked_at: ???
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
        index_content: &TarballIndex,
    ) -> Result<(), WriteTarballIndexFileError> {
        let file_path = self.tarball_index_file_path(tarball_integrity);
        let index_content =
            serde_json::to_string(&index_content).expect("convert a TarballIndex to JSON");
        ensure_file(&file_path, index_content.as_bytes())
            .map_err(WriteTarballIndexFileError::WriteFile)
    }
}
