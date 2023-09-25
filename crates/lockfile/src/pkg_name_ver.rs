use derive_more::{Display, Error};
use node_semver::{SemverError, Version};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Syntax: `{name}@{version}`
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[display(fmt = "{name}@{version}")]
#[serde(try_from = "&'de str", into = "String")]
pub struct PkgNameVer {
    pub name: String,
    pub version: Version,
}

/// Error when parsing [`PkgNameVer`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParsePkgNameVerError {
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
        let (name, version) = value.split_once('@').ok_or(ParsePkgNameVerError::MissingAtSign)?;
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
