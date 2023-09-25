use crate::PkgNameVerPeer;
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
/// * `/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
/// * `registry.npmjs.com/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
/// * `registry.node-modules.io/ts-node@10.9.1(@types/node@18.7.19)(typescript@5.1.6)`
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[display(fmt = "{}/{package_specifier}", "custom_registry.as_deref().unwrap_or_default()")]
#[serde(try_from = "&'de str", into = "String")]
pub struct DependencyPath {
    pub custom_registry: Option<String>,
    pub package_specifier: PkgNameVerPeer,
}

/// Error when parsing [`DependencyPath`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseDependencyPathError {
    #[display(fmt = "Invalid syntax")]
    InvalidSyntax,
}

impl FromStr for DependencyPath {
    type Err = ParseDependencyPathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (custom_registry, package_specifier) =
            s.split_once('/').ok_or(ParseDependencyPathError::InvalidSyntax)?;
        let custom_registry =
            if custom_registry.is_empty() { None } else { Some(custom_registry.to_string()) };
        let package_specifier =
            package_specifier.parse().map_err(|_| ParseDependencyPathError::InvalidSyntax)?;
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
        macro_rules! case {
            ($custom_registry:expr, $package_specifier:expr => $output:expr) => {{
                let custom_registry = $custom_registry.map(|x: &str| x.to_string());
                let package_specifier = $package_specifier.parse().unwrap();
                eprintln!("TEST: {custom_registry:?}, {package_specifier:?}");
                let yaml =
                    serde_yaml::to_string(&DependencyPath { custom_registry, package_specifier })
                        .unwrap();
                assert_eq!(yaml.trim(), $output);
            }};
        }

        case!(None, "foo@1.0.0" => "/foo@1.0.0");
        case!(Some("registry.node-modules.io"), "foo@1.0.0" => "registry.node-modules.io/foo@1.0.0");
    }

    #[test]
    fn deserialize() {
        macro_rules! case {
            ($input:expr => $custom_registry:expr, $package_specifier:expr) => {{
                let input = $input;
                eprintln!("TEST: {input:?}");
                let dependency_path: DependencyPath = serde_yaml::from_str(input).unwrap();
                assert_eq!(
                    dependency_path,
                    DependencyPath {
                        custom_registry: $custom_registry.map(|x: &str| x.to_string()),
                        package_specifier: $package_specifier.parse().unwrap(),
                    }
                );
            }};
        }

        case!("/foo@1.0.0" => None, "foo@1.0.0");
        case!("registry.node-modules.io/foo@1.0.0" => Some("registry.node-modules.io"), "foo@1.0.0");
    }

    #[test]
    fn parse_error() {
        let error = "foo@1.0.0".parse::<DependencyPath>().unwrap_err();
        assert_eq!(error.to_string(), "Invalid syntax");
        assert!(matches!(error, ParseDependencyPathError::InvalidSyntax));
    }
}
