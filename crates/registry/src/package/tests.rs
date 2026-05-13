use std::collections::HashMap;

use node_semver::Version;
use pretty_assertions::assert_eq;

use super::{AuthHeaders, Package, PackageVersion, ThrottledClient};
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

/// [`Package::fetch_from_registry`] must attach the registry-keyed
/// `Authorization` header on every metadata GET, even for the
/// abbreviated install-v1 endpoint. `mockito::Matcher::Exact`
/// rejects the request unless the header arrives verbatim, so a
/// missing or wrong header would 501 the request and propagate as
/// a deserialization error.
#[tokio::test]
async fn fetch_from_registry_attaches_authorization_header() {
    let mut server = mockito::Server::new_async().await;
    let body = r#"{"name":"acme","dist-tags":{"latest":"1.0.0"},"versions":{}}"#;
    let mock = server
        .mock("GET", "/acme")
        .match_header("authorization", "Bearer top-secret")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .expect(1)
        .create_async()
        .await;

    let registry = format!("{}/", server.url());
    let client = ThrottledClient::default();
    let auth_headers = AuthHeaders::from_creds_map(
        [(pacquet_network::nerf_dart(&registry), "Bearer top-secret".to_owned())],
        None,
    );

    let pkg = Package::fetch_from_registry("acme", &client, &registry, &auth_headers)
        .await
        .expect("server should accept the request once the bearer header is attached");
    assert_eq!(pkg.name, "acme");
    mock.assert_async().await;
}
