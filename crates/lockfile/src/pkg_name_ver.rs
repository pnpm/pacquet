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
    #[display(fmt = "At sign (@) is missing")]
    MissingAtSign,
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
                    rest.split_once('@').ok_or(ParsePkgNameVerError::MissingAtSign)?;
                let name = &value[..name_without_at.len() + 1];
                debug_assert_eq!(name, format!("@{name_without_at}"));
                (name, version)
            }
            Some((_, _)) => value.split_once('@').ok_or(ParsePkgNameVerError::MissingAtSign)?,
        };
        if name.is_empty() {
            return Err(ParsePkgNameVerError::EmptyName);
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
