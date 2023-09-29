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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn assert_ver_peer<Ver, Peer>(received: PkgVerPeer, expected_version: Ver, expected_peer: Peer)
    where
        Ver: Into<Version>,
        Peer: Into<String>,
    {
        dbg!(&received);
        let expected_version = expected_version.into();
        let expected_peer = expected_peer.into();
        assert_eq!(
            (received.version(), received.peer()),
            (&expected_version, expected_peer.as_str()),
        );
        assert_eq!(received.into_tuple(), (expected_version, expected_peer));
    }

    fn decode_encode_case<Decode, Encode>(input: &str, decode: Decode, encode: Encode)
    where
        Decode: Fn(&str) -> PkgVerPeer,
        Encode: Fn(&PkgVerPeer) -> String,
    {
        eprintln!("CASE: {input:?}");
        let peer_ver = decode(input);
        dbg!(&peer_ver);
        let output = encode(&peer_ver);
        assert_eq!(input, output);
    }

    #[test]
    fn parse_ok() {
        macro_rules! case {
            ($input:expr => $expected_version:expr, $expected_peer:expr) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                assert_ver_peer(input.parse().unwrap(), $expected_version, $expected_peer);
            }};
        }

        case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => (1, 21, 3), "(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case!("1.21.3(react@17.0.2)" => (1, 21, 3), "(react@17.0.2)");
        case!("1.21.3-rc.0(react@17.0.2)" => "1.21.3-rc.0".parse::<Version>().unwrap(), "(react@17.0.2)");
        case!("1.21.3" => (1, 21, 3), "");
        case!("1.21.3-rc.0" => "1.21.3-rc.0".parse::<Version>().unwrap(), "");
    }

    #[test]
    fn parse_err() {
        macro_rules! case {
            ($input:expr => $message:expr, $variant:pat) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let error = input.parse::<PkgVerPeer>().unwrap_err();
                dbg!(&error);
                assert_eq!(error.to_string(), $message);
                assert!(matches!(error, $variant));
            }};
        }
        case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
        case!("1.21.3(" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
        case!("1.21.3)" => "Mismatch parenthesis", ParsePkgVerPeerError::MismatchParenthesis);
        case!("a.b.c" => "Failed to parse the version part: Failed to parse version.", ParsePkgVerPeerError::ParseVersionFailure(_));
    }

    #[test]
    fn deserialize_ok() {
        macro_rules! case {
            ($input:expr => $expected_version:expr, $expected_peer:expr) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                assert_ver_peer(
                    serde_yaml::from_str(input).unwrap(),
                    $expected_version,
                    $expected_peer,
                );
            }};
        }

        case!("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => (1, 21, 3), "(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case!("1.21.3(react@17.0.2)" => (1, 21, 3), "(react@17.0.2)");
        case!("1.21.3-rc.0(react@17.0.2)" => "1.21.3-rc.0".parse::<Version>().unwrap(), "(react@17.0.2)");
        case!("1.21.3" => (1, 21, 3), "");
        case!("1.21.3-rc.0" => "1.21.3-rc.0".parse::<Version>().unwrap(), "");
    }

    #[test]
    fn parse_to_string() {
        let case =
            |input| decode_encode_case(input, |input| input.parse().unwrap(), ToString::to_string);
        case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("1.21.3(react@17.0.2)");
        case("1.21.3-rc.0(react@17.0.2)");
        case("1.21.3");
        case("1.21.3-rc.0");
    }

    #[test]
    fn deserialize_serialize() {
        let case = |input| {
            decode_encode_case(
                input,
                |input| serde_yaml::from_str(input).unwrap(),
                |ver_peer| serde_yaml::to_string(&ver_peer).unwrap().trim().to_string(),
            )
        };
        case("1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)");
        case("1.21.3(react@17.0.2)");
        case("1.21.3-rc.0(react@17.0.2)");
        case("1.21.3");
        case("1.21.3-rc.0");
    }
}
