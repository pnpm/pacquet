use serde::{Deserialize, Serialize};
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
