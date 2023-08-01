use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
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

impl PackageDistribution {
    pub fn empty() -> Self {
        PackageDistribution {
            integrity: "".to_string(),
            npm_signature: None,
            shasum: "".to_string(),
            tarball: "".to_string(),
            file_count: Some(0),
            unpacked_size: Some(0),
        }
    }
}
