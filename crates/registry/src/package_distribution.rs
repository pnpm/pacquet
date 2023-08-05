use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Default, Clone, Eq)]
pub struct PackageDistribution {
    pub integrity: String,
    pub shasum: String,
    pub tarball: String,
    #[serde(alias = "fileCount")]
    pub file_count: Option<usize>,
    #[serde(alias = "unpackedSize")]
    pub unpacked_size: Option<usize>,
}

impl PartialEq for PackageDistribution {
    fn eq(&self, other: &Self) -> bool {
        self.integrity == other.integrity
    }
}
