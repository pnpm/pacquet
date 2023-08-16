use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Default, Clone, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PackageDistribution {
    pub integrity: String, // TODO: why fetching typescript cause error here?
    pub shasum: String,
    pub tarball: String,
    pub file_count: Option<usize>,
    pub unpacked_size: Option<usize>,
}

impl PartialEq for PackageDistribution {
    fn eq(&self, other: &Self) -> bool {
        self.integrity == other.integrity
    }
}
