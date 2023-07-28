use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct PackageDistribution {
    pub integrity: String,
    #[serde(alias = "npm-signature")]
    pub npm_signature: Option<String>,
    pub shasum: String,
    pub tarball: String,
}

impl PackageDistribution {
    pub fn empty() -> Self {
        PackageDistribution {
            integrity: "".to_string(),
            npm_signature: None,
            shasum: "".to_string(),
            tarball: "".to_string(),
        }
    }
}
