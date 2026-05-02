use std::collections::HashMap;

use node_semver::Version;
use pretty_assertions::assert_eq;

use super::PackageVersion;
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

    let dependencies = |peer| version.dependencies(peer).collect::<HashMap<_, _>>();
    assert!(dependencies(false).contains_key("fastify"));
    assert!(!dependencies(false).contains_key("fast-querystring"));
    assert!(dependencies(true).contains_key("fastify"));
    assert!(dependencies(true).contains_key("fast-querystring"));
    assert!(!dependencies(true).contains_key("hello-world"));
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
