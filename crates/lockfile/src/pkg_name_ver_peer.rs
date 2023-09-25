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

/// Error when parsing [`PkgVerPeer`] from a string.
pub type ParsePkgNameVerPeerError = ParsePkgNameSuffixError<Infallible>;
