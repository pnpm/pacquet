use crate::{ParsePkgNameVerPeerError, PkgNameVerPeer};
use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Dependency path is the key of the `packages` map.
///
/// Specification: <https://github.com/pnpm/spec/blob/master/lockfile/6.0.md#packages>
///
/// Syntax: `{custom_registry}/{package_specifier}`
///
/// Syntax Examples:
/// * `/ts-node@10.9.1`
/// * `registry.npmjs.com/ts-node@10.9.1`
/// * `registry.node-modules.io/ts-node@10.9.1`
/// * `/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
/// * `registry.npmjs.com/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
/// * `registry.node-modules.io/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[display(fmt = "{}/{package_specifier}", "custom_registry.as_deref().unwrap_or_default()")]
#[serde(try_from = "&'de str", into = "String")]
pub struct DependencyPath {
    pub custom_registry: Option<String>,
    pub package_specifier: PkgNameVerPeer, // TODO: add support for `{registry}/{name}/{version}({peers})` syntax
}

/// Error when parsing [`DependencyPath`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseDependencyPathError {
    #[display(fmt = "Invalid syntax")]
    InvalidSyntax,
    #[display(fmt = "Failed to parse specifier: {_0}")]
    ParsePackageSpecifierFailure(ParsePkgNameVerPeerError),
}

impl FromStr for DependencyPath {
    type Err = ParseDependencyPathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (custom_registry, package_specifier) =
            s.split_once('/').ok_or(ParseDependencyPathError::InvalidSyntax)?;
        let custom_registry =
            if custom_registry.is_empty() { None } else { Some(custom_registry.to_string()) };
        let package_specifier = package_specifier
            .parse()
            .map_err(ParseDependencyPathError::ParsePackageSpecifierFailure)?;
        Ok(DependencyPath { custom_registry, package_specifier })
    }
}

impl<'a> TryFrom<&'a str> for DependencyPath {
    type Error = ParseDependencyPathError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<DependencyPath> for String {
    fn from(value: DependencyPath) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn serialize() {
        fn case(
            (custom_registry, package_specifier): (Option<&'static str>, &'static str),
            output: &'static str,
        ) {
            eprintln!("CASE: {custom_registry:?}, {package_specifier:?}");
            let custom_registry = custom_registry.map(ToString::to_string);
            let package_specifier = package_specifier.parse().unwrap();
            let yaml =
                serde_yaml::to_string(&DependencyPath { custom_registry, package_specifier })
                    .unwrap();
            assert_eq!(yaml.trim(), output);
        }

        case((None, "ts-node@10.9.1"), "/ts-node@10.9.1");
        case(
            (Some("registry.node-modules.io"), "ts-node@10.9.1"),
            "registry.node-modules.io/ts-node@10.9.1",
        );
        case(
            (None, "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)"),
            "/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
        );
        case(
            (
                Some("registry.node-modules.io"),
                "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            ),
            "registry.node-modules.io/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
        );
        case(
            (None, "@babel/plugin-proposal-object-rest-spread@7.12.1"),
            "/@babel/plugin-proposal-object-rest-spread@7.12.1",
        );
        case(
            (Some("registry.node-modules.io"), "@babel/plugin-proposal-object-rest-spread@7.12.1"),
            "registry.node-modules.io/@babel/plugin-proposal-object-rest-spread@7.12.1",
        );
        case(
            (None, "@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)"),
            "/@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
        );
        case(
            (
                Some("registry.node-modules.io"),
                "@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
            ),
            "registry.node-modules.io/@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
        );
    }

    #[test]
    fn deserialize() {
        fn case(
            input: &'static str,
            (custom_registry, package_specifier): (Option<&'static str>, &'static str),
        ) {
            eprintln!("CASE: {input:?}");
            let dependency_path: DependencyPath = serde_yaml::from_str(input).unwrap();
            assert_eq!(
                dependency_path,
                DependencyPath {
                    custom_registry: custom_registry.map(|x: &str| x.to_string()),
                    package_specifier: package_specifier.parse().unwrap(),
                }
            );
        }

        case("/ts-node@10.9.1", (None, "ts-node@10.9.1"));
        case(
            "registry.node-modules.io/ts-node@10.9.1",
            (Some("registry.node-modules.io"), "ts-node@10.9.1"),
        );
        case(
            "/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            (None, "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)"),
        );
        case(
            "registry.node-modules.io/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            (
                Some("registry.node-modules.io"),
                "ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)",
            ),
        );
        case(
            "/@babel/plugin-proposal-object-rest-spread@7.12.1",
            (None, "@babel/plugin-proposal-object-rest-spread@7.12.1"),
        );
        case(
            "registry.node-modules.io/@babel/plugin-proposal-object-rest-spread@7.12.1",
            (Some("registry.node-modules.io"), "@babel/plugin-proposal-object-rest-spread@7.12.1"),
        );
        case(
            "/@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
            (None, "@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)"),
        );
        case(
            "registry.node-modules.io/@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
            (
                Some("registry.node-modules.io"),
                "@babel/plugin-proposal-object-rest-spread@7.12.1(@babel/core@7.12.9)",
            ),
        );
    }

    #[test]
    fn parse_error() {
        let error = "ts-node@10.9.1".parse::<DependencyPath>().unwrap_err();
        assert_eq!(error.to_string(), "Invalid syntax");
        assert!(matches!(error, ParseDependencyPathError::InvalidSyntax));
    }
}
