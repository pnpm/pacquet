use std::collections::HashMap;

use pipe_trait::Pipe;
use serde::{Deserialize, Serialize};

use crate::package_distribution::PackageDistribution;
use crate::RegistryError;

#[derive(Serialize, Deserialize, Debug, Clone, Eq)]
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

impl PartialEq for PackageVersion {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist
    }
}

impl PackageVersion {
    pub async fn fetch_from_registry(
        name: &str,
        version: &str,
        http_client: &reqwest::Client,
        registry: &str,
    ) -> Result<Self, RegistryError> {
        http_client
            .get(format!("{0}{name}/{version}", &registry))
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<PackageVersion>()
            .await?
            .pipe(Ok)
    }

    pub fn get_store_name(&self) -> String {
        format!("{0}@{1}", self.name.replace('/', "+"), self.version)
    }

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

    pub fn serialize(&self, save_exact: bool) -> String {
        let prefix = if save_exact { "" } else { "^" };
        format!("{0}{1}", prefix, self.version)
    }
}
