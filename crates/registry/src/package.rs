use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{package_version::PackageVersion, RegistryError};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Package {
    pub name: String,
    #[serde(rename = "dist-tags")]
    dist_tags: HashMap<String, String>,
    pub versions: HashMap<String, PackageVersion>,

    #[serde(skip_serializing, skip_deserializing)]
    pub mutex: Arc<Mutex<u8>>,
}

impl PartialEq for Package {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Package {
    pub async fn fetch_from_registry(
        name: &str,
        http_client: &reqwest::Client,
        registry: &str,
    ) -> Result<Self, RegistryError> {
        Ok(http_client
            .get(format!("{0}{name}", &registry))
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<Package>()
            .await?)
    }

    pub fn get_pinned_version(
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

    pub fn get_latest(&self) -> Result<&PackageVersion, RegistryError> {
        let version =
            self.dist_tags.get("latest").expect("latest tag is expected but not found for package");
        Ok(self.versions.get(version).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use node_semver::Version;

    use super::*;
    use crate::package_distribution::PackageDistribution;

    #[test]
    pub fn package_version_should_include_peers() {
        let mut dependencies = HashMap::<String, String>::new();
        dependencies.insert("fastify".to_string(), "1.0.0".to_string());
        let mut peer_dependencies = HashMap::<String, String>::new();
        peer_dependencies.insert("fast-querystring".to_string(), "1.0.0".to_string());
        let version = PackageVersion {
            name: "".to_string(),
            version: Version::parse("1.0.0").unwrap(),
            dist: PackageDistribution::default(),
            dependencies: Some(dependencies),
            dev_dependencies: None,
            peer_dependencies: Some(peer_dependencies),
        };

        assert!(version.get_dependencies(false).contains_key("fastify"));
        assert!(!version.get_dependencies(false).contains_key("fast-querystring"));
        assert!(version.get_dependencies(true).contains_key("fastify"));
        assert!(version.get_dependencies(true).contains_key("fast-querystring"));
        assert!(!version.get_dependencies(true).contains_key("hello-world"));
    }

    #[test]
    pub fn serialized_according_to_params() {
        let version = PackageVersion {
            name: "".to_string(),
            version: Version { major: 3, minor: 2, patch: 1, build: vec![], pre_release: vec![] },
            dist: PackageDistribution::default(),
            dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
        };

        assert_eq!(version.serialize(true), "3.2.1");
        assert_eq!(version.serialize(false), "^3.2.1");
    }
}
