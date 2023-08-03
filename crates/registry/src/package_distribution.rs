use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct PackageDistribution {
    pub integrity: String,
    #[serde(alias = "npm-signature")]
    pub npm_signature: Option<String>,
    pub shasum: String,
    pub tarball: String,
    #[serde(alias = "fileCount")]
    pub file_count: Option<usize>,
    #[serde(alias = "unpackedSize")]
    pub unpacked_size: Option<usize>,
}
