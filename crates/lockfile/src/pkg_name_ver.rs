use derive_more::{Display, Error};
use node_semver::{SemverError, Version};
use serde::{Deserialize, Serialize};
use split_first_char::SplitFirstChar;
use std::str::FromStr;

/// Syntax: `{name}@{version}`
///
/// Examples: `ts-node@10.9.1`, `@types/node@18.7.19`, `typescript@5.1.6`
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[display(fmt = "{name}@{version}")]
#[serde(try_from = "&'de str", into = "String")]
pub struct PkgNameVer {
    pub name: String,
    pub version: Version,
}

impl PkgNameVer {
    /// Construct a [`PkgNameVer`].
    pub fn new(name: impl Into<String>, version: impl Into<Version>) -> Self {
        PkgNameVer { name: name.into(), version: version.into() }
    }
}

/// Error when parsing [`PkgNameVer`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParsePkgNameVerError {
    #[display(fmt = "Input is empty")]
    EmptyInput,
    #[display(fmt = "Version is missing")]
    MissingVersion,
    #[display(fmt = "Name is empty")]
    EmptyName,
    #[display(fmt = "Failed to parse version: {_0}")]
    ParseVersionFailure(#[error(source)] SemverError),
}

impl FromStr for PkgNameVer {
    type Err = ParsePkgNameVerError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, version) = match value.split_first_char() {
            None => return Err(ParsePkgNameVerError::EmptyInput),
            Some(('@', rest)) => {
                let (name_without_at, version) =
                    rest.split_once('@').ok_or(ParsePkgNameVerError::MissingVersion)?;
                let name = &value[..name_without_at.len() + 1];
                debug_assert_eq!(name, format!("@{name_without_at}"));
                (name, version)
            }
            Some((_, _)) => value.split_once('@').ok_or(ParsePkgNameVerError::MissingVersion)?,
        };
        if matches!(name, "" | "@" | "@/") {
            return Err(ParsePkgNameVerError::EmptyName);
        }
        if version.is_empty() {
            return Err(ParsePkgNameVerError::MissingVersion);
        }
        let version =
            version.parse::<Version>().map_err(ParsePkgNameVerError::ParseVersionFailure)?;
        let name = name.to_string();
        Ok(PkgNameVer { name, version })
    }
}

impl<'a> TryFrom<&'a str> for PkgNameVer {
    type Error = ParsePkgNameVerError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<PkgNameVer> for String {
    fn from(value: PkgNameVer) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use serde_yaml::Value as YamlValue;

    #[test]
    fn parse_ok() {
        macro_rules! case {
            ($input:expr => $output:expr) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let received: PkgNameVer = input.parse().unwrap();
                let expected = $output;
                assert_eq!(&received, &expected);
            }};
        }

        case!("ts-node@10.9.1" => PkgNameVer::new("ts-node", (10, 9, 1)));
        case!("@types/node@18.7.19" => PkgNameVer::new("@types/node", (18, 7, 19)));
        case!("typescript@5.1.6" => PkgNameVer::new("typescript", (5, 1, 6)));
    }

    #[test]
    fn deserialize_ok() {
        macro_rules! case {
            ($input:expr => $output:expr) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let received: PkgNameVer = serde_yaml::from_str(input).unwrap();
                let expected = $output;
                assert_eq!(&received, &expected);
            }};
        }

        case!("ts-node@10.9.1" => PkgNameVer::new("ts-node", (10, 9, 1)));
        case!("'@types/node@18.7.19'" => PkgNameVer::new("@types/node", (18, 7, 19)));
        case!("typescript@5.1.6" => PkgNameVer::new("typescript", (5, 1, 6)));
    }

    #[test]
    fn parse_err() {
        macro_rules! case {
            ($title:literal: $input:expr => $message:expr, $pattern:pat) => {{
                let title = $title;
                let input = $input;
                eprintln!("CASE: {title} (input = {input:?})");
                let error = input.parse::<PkgNameVer>().unwrap_err();
                dbg!(&error);
                assert_eq!(error.to_string(), $message);
                assert!(matches!(&error, $pattern));
            }};
        }

        case!("Empty input": "" => "Input is empty", ParsePkgNameVerError::EmptyInput);
        case!("Non-scope name without version": "ts-node" => "Version is missing", ParsePkgNameVerError::MissingVersion);
        case!("Scoped name without version": "@types/node" => "Version is missing", ParsePkgNameVerError::MissingVersion);
        case!("Non-scope name with empty version": "ts-node" => "Version is missing", ParsePkgNameVerError::MissingVersion);
        case!("Scoped name with empty version": "@types/node" => "Version is missing", ParsePkgNameVerError::MissingVersion);
        case!("Missing name": "10.9.1" => "Version is missing", ParsePkgNameVerError::MissingVersion); // can't fix without parser combinator
        case!("Empty non-scope name": "@19.9.1" => "Version is missing", ParsePkgNameVerError::MissingVersion); // can't fix without parser combinator
        case!("Empty scoped name": "@@18.7.19" => "Name is empty", ParsePkgNameVerError::EmptyName);
    }

    #[test]
    fn to_string() {
        let string = PkgNameVer::new("ts-node", (10, 9, 1)).to_string();
        assert_eq!(string, "ts-node@10.9.1");
    }

    #[test]
    fn serialize() {
        let received =
            PkgNameVer::new("ts-node", (10, 9, 1)).pipe_ref(serde_yaml::to_value).unwrap();
        let expected = "ts-node@10.9.1".to_string().pipe(YamlValue::String);
        assert_eq!(received, expected);
    }
}
