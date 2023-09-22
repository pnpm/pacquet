use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::{fmt::Display, ops::Deref, str::FromStr};

/// Dependency path is the key of the `packages` map.
///
/// Specification: https://github.com/pnpm/spec/blob/master/lockfile/6.0.md#packages
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[display(
    fmt = "{}/{}",
    r#"custom_registry.as_deref().unwrap_or("")"#,
    "package_specifier.deref()"
)]
#[display(bound = "Text: Deref<Target = str>")]
#[serde(
    try_from = "&'de str",
    into = "String",
    bound(
        deserialize = "Self: TryFrom<&'de str>, <Self as TryFrom<&'de str>>::Error: Display",
        serialize = "Text: Clone + Deref<Target = str>"
    )
)]
pub struct DependencyPath<Text> {
    pub custom_registry: Option<Text>,
    pub package_specifier: Text,
}

impl<Text> DependencyPath<Text> {
    /// Convert the internal fields to an owned variant.
    pub fn to_owned<Owned>(&self) -> DependencyPath<Owned>
    where
        Text: Deref,
        Text::Target: ToOwned<Owned = Owned>,
    {
        let DependencyPath { custom_registry, package_specifier } = self;
        DependencyPath {
            custom_registry: custom_registry.as_deref().map(ToOwned::to_owned),
            package_specifier: package_specifier.deref().to_owned(),
        }
    }

    /// Convert the internal fields to references.
    pub fn as_ref<Target: ?Sized>(&self) -> DependencyPath<&'_ Target>
    where
        Text: AsRef<Target>,
    {
        let DependencyPath { custom_registry, package_specifier } = self;
        DependencyPath {
            custom_registry: custom_registry.as_ref().map(AsRef::as_ref),
            package_specifier: package_specifier.as_ref(),
        }
    }
}

/// Error when parsing [`DependencyPath`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseDependencyPathError {
    #[display(fmt = "Invalid syntax")]
    InvalidSyntax,
}

impl FromStr for DependencyPath<String> {
    type Err = ParseDependencyPathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DependencyPath::<&str>::try_from(s).map(|x| x.to_owned())
    }
}

impl TryFrom<String> for DependencyPath<String> {
    type Error = ParseDependencyPathError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl<'a> TryFrom<&'a str> for DependencyPath<&'a str> {
    type Error = ParseDependencyPathError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        let (custom_registry, package_specifier) =
            value.split_once('/').ok_or(ParseDependencyPathError::InvalidSyntax)?;
        let custom_registry = if custom_registry.is_empty() { None } else { Some(custom_registry) };
        Ok(DependencyPath { custom_registry, package_specifier })
    }
}

impl<Text> From<DependencyPath<Text>> for String
where
    Text: Deref<Target = str>,
{
    fn from(value: DependencyPath<Text>) -> Self {
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
                let custom_registry = $custom_registry;
                let package_specifier = $package_specifier;
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
                let dependency_path: DependencyPath<&str> = serde_yaml::from_str(input).unwrap();
                assert_eq!(
                    dependency_path,
                    DependencyPath {
                        custom_registry: $custom_registry,
                        package_specifier: $package_specifier,
                    }
                );
            }};
        }

        case!("/foo@1.0.0" => None, "foo@1.0.0");
        case!("registry.node-modules.io/foo@1.0.0" => Some("registry.node-modules.io"), "foo@1.0.0");
    }

    #[test]
    fn parse_error() {
        let error = DependencyPath::<&str>::try_from("foo@1.0.0").unwrap_err();
        assert_eq!(error.to_string(), "Invalid syntax");
        assert!(matches!(error, ParseDependencyPathError::InvalidSyntax));
    }
}
