use derive_more::{Display, Error};
use node_semver::{SemverError, Version};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Suffix type of [`PkgNameVerPeer`](crate::PkgNameVerPeer) and
/// type of [`ResolvedDependencySpec::version`](crate::ResolvedDependencySpec::version).
///
/// Example: `1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)`
///
/// **NOTE:** The peer part isn't guaranteed to be correct. It is only assumed to be.
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[display(fmt = "{version}{peer}")]
#[serde(try_from = "&'de str", into = "String")]
pub struct PkgVerPeer {
    version: Version,
    peer: String,
}

impl PkgVerPeer {
    /// Get the version part.
    pub fn version(&self) -> &'_ Version {
        &self.version
    }

    /// Get the peer part.
    pub fn peer(&self) -> &'_ str {
        self.peer.as_str()
    }

    /// Destructure the struct into a tuple of version and peer.
    pub fn into_tuple(self) -> (Version, String) {
        let PkgVerPeer { version, peer } = self;
        (version, peer)
    }
}

/// Error when parsing [`PkgVerPeer`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParsePkgVerPeerError {
    #[display(fmt = "Failed to parse the version part: {_0}")]
    ParseVersionFailure(#[error(source)] SemverError),
    #[display(fmt = "Mismatch parenthesis")]
    MismatchParenthesis,
}

impl FromStr for PkgVerPeer {
    type Err = ParsePkgVerPeerError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !value.ends_with(')') {
            if value.find(|char| char == '(' || char == ')').is_some() {
                return Err(ParsePkgVerPeerError::MismatchParenthesis);
            }

            let version = value.parse().map_err(ParsePkgVerPeerError::ParseVersionFailure)?;
            return Ok(PkgVerPeer { version, peer: String::new() });
        }

        let opening_parenthesis =
            value.find('(').ok_or(ParsePkgVerPeerError::MismatchParenthesis)?;
        let version = value[..opening_parenthesis]
            .parse()
            .map_err(ParsePkgVerPeerError::ParseVersionFailure)?;
        let peer = value[opening_parenthesis..].to_string();
        Ok(PkgVerPeer { version, peer })
    }
}

impl<'a> TryFrom<&'a str> for PkgVerPeer {
    type Error = ParsePkgVerPeerError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<PkgVerPeer> for String {
    fn from(value: PkgVerPeer) -> Self {
        value.to_string()
    }
}
