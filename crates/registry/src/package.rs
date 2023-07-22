use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::RegistryError;

#[derive(Serialize, Deserialize, Debug)]
pub struct PackageDistribution {
    pub integrity: String,
    #[serde(alias = "npm-signature")]
    pub npm_signature: Option<String>,
    pub shasum: String,
    pub tarball: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PackageVersion {
    pub name: String,
    pub version: node_semver::Version,
    pub dist: PackageDistribution,
    pub dependencies: Option<HashMap<String, String>>,
    #[serde(alias = "devDependencies")]
    pub dev_dependencies: Option<HashMap<String, String>>,
    #[serde(alias = "peerDependencies")]
    pub peer_dependencies: Option<HashMap<String, String>>,
}

impl PackageVersion {
    pub fn get_tarball_url(&self) -> &str {
        self.dist.tarball.as_str()
    }

    pub fn get_dependencies(&self, with_peer_dependencies: bool) -> HashMap<&str, &str> {
        let mut dependencies = HashMap::<&str, &str>::new();

        if let Some(deps) = self.dependencies.as_ref() {
            for dep in deps {
                dependencies.insert(dep.0.as_str(), dep.1.as_str());
            }
        }

        if with_peer_dependencies {
            if let Some(deps) = self.peer_dependencies.as_ref() {
                for dep in deps {
                    dependencies.insert(dep.0.as_str(), dep.1.as_str());
                }
            }
        }

        dependencies
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Package {
    pub name: String,
    #[serde(alias = "dist-tags")]
    dist_tags: HashMap<String, String>,
    pub versions: HashMap<String, PackageVersion>,

    #[serde(skip_serializing, skip_deserializing)]
    pub mutex: Arc<Mutex<u8>>,
}

impl Package {
    pub fn get_suitable_version_of(
        &self,
        version_field: &str,
    ) -> Result<Option<&PackageVersion>, RegistryError> {
        let range: node_semver::Range = version_field.parse().unwrap();
        let mut satisfied_versions = self
            .versions
            .values()
            .filter(|v| v.version.satisfies(&range))
            .collect::<Vec<&PackageVersion>>()
            .clone();

        satisfied_versions.sort_by(|a, b| a.version.partial_cmp(&b.version).unwrap());

        // Optimization opportunity:
        // We can store this in a cache to remove filter operation and make this a O(1) operation.
        Ok(satisfied_versions.last().copied())
    }
}
