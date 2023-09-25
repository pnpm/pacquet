use crate::{ParsePkgNameSuffixError, PkgNameSuffix};
use derive_more::{AsRef, Deref, Display};
use pipe_trait::Pipe;
use std::{convert::Infallible, str::FromStr};

/// Suffix type of [`PkgNameVerPeer`].
///
/// Example: `1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)`
///
/// **NOTE:** The internal string isn't guaranteed to be correct. It is only assumed to be.
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, AsRef, Deref)]
pub struct PkgVerPeer(String);

impl FromStr for PkgVerPeer {
    type Err = Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.to_string().pipe(PkgVerPeer).pipe(Ok)
    }
}

/// Syntax: `{name}@{version}({peers})`
///
/// Example: `react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)`
///
/// **NOTE:** The suffix isn't guaranteed to be correct. It is only assumed to be.
pub type PkgNameVerPeer = PkgNameSuffix<PkgVerPeer>;

/// Error when parsing [`PkgNameVerPeer`] from a string.
pub type ParsePkgNameVerPeerError = ParsePkgNameSuffixError<Infallible>;

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;

    fn name_peer_ver(name: &str, peer_ver: &str) -> PkgNameVerPeer {
        let peer_ver = peer_ver.to_string().pipe(PkgVerPeer);
        PkgNameVerPeer::new(name, peer_ver)
    }

    #[test]
    fn parse() {
        macro_rules! case {
            ($input:expr => $output:expr) => {{
                let input = $input;
                eprintln!("CASE: {input:?}");
                let received: PkgNameVerPeer = input.parse().unwrap();
                let expected = $output;
                assert_eq!(&received, &expected);
            }};
        }

        case!(
            "react-json-view@1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)" => name_peer_ver(
                "react-json-view",
                "1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)",
            )
        );
        case!("react-json-view@1.21.3" => name_peer_ver("react-json-view", "1.21.3"));
        case!(
            "@algolia/autocomplete-core@1.9.3(@algolia/client-search@4.18.0)(algoliasearch@4.18.0)(search-insights@2.6.0)" => name_peer_ver(
                "@algolia/autocomplete-core",
                "1.9.3(@algolia/client-search@4.18.0)(algoliasearch@4.18.0)(search-insights@2.6.0)",
            )
        );
        case!("@algolia/autocomplete-core@1.9.3" => name_peer_ver("@algolia/autocomplete-core", "1.9.3"));
    }
}
