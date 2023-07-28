use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{package_version::PackageVersion, RegistryError};

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
            dist: PackageDistribution::empty(),
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
            dist: PackageDistribution {
                integrity: "".to_string(),
                npm_signature: None,
                shasum: "".to_string(),
                tarball: "".to_string(),
            },
            dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
        };

        assert_eq!(version.serialize(true), "3.2.1");
        assert_eq!(version.serialize(false), "^3.2.1");
    }
}
