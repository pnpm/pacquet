use derive_more::{Display, Error};
use node_semver::{SemverError, Version};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, str::FromStr};

/// Suffix type of [`PkgNameVerPeer`](crate::PkgNameVerPeer) and
/// type of [`ResolvedDependencySpec::version`](crate::ResolvedDependencySpec::version).
///
/// Example: `1.21.3(@types/react@17.0.49)(react-dom@17.0.2)(react@17.0.2)`
///
/// Also accepts an optional `runtime:` prefix for pnpm v11 runtime
/// dependencies (`node@runtime:22.0.0`, `deno@runtime:1.x`,
/// `bun@runtime:1`). The prefix is preserved through `Display` so
/// the round-trip stays byte-stable. Mirrors upstream's depPath
/// shape for `BinaryResolution` / `VariationsResolution` entries
/// emitted by the runtime resolvers at
/// <https://github.com/pnpm/pnpm/blob/94240bc046/engine/runtime>.
///
/// **NOTE:** The peer part isn't guaranteed to be correct. It is only assumed to be.
#[derive(Debug, Display, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[display("{}{version}{peer}", prefix.as_deref().unwrap_or(""))]
#[serde(try_from = "Cow<'de, str>", into = "String")]
pub struct PkgVerPeer {
    /// `Some("runtime:")` for runtime depPaths, `None` for plain
    /// semver. Preserved through `Display` so a round-trip
    /// produces byte-stable output for the lockfile.
    prefix: Option<String>,
    version: Version,
    peer: String,
}

/// `runtime:` is the only scheme prefix pacquet currently accepts.
/// Defined here so call sites can match on it without re-spelling
/// the literal everywhere. Other schemes (`tag:`, etc.) can be
/// added later — see #511's "Out of scope" note.
pub const RUNTIME_PREFIX: &str = "runtime:";

impl PkgVerPeer {
    /// Get the version part.
    pub fn version(&self) -> &'_ Version {
        &self.version
    }

    /// Get the peer part.
    pub fn peer(&self) -> &'_ str {
        self.peer.as_str()
    }

    /// Get the optional scheme prefix (e.g. `Some("runtime:")` for
    /// `runtime:22.0.0`). Returns `None` for plain semver.
    pub fn prefix(&self) -> Option<&'_ str> {
        self.prefix.as_deref()
    }

    /// `true` when the prefix is `runtime:` — i.e. this entry
    /// resolves through one of pnpm v11's runtime resolvers
    /// (`node`/`deno`/`bun`). Typed replacement for the
    /// `depPath.contains("@runtime:")` substring check upstream
    /// uses at
    /// <https://github.com/pnpm/pnpm/blob/94240bc046/installing/deps-installer/src/install/index.ts#L1374-L1387>.
    pub fn is_runtime(&self) -> bool {
        self.prefix.as_deref() == Some(RUNTIME_PREFIX)
    }

    /// Destructure the struct into a tuple of version and peer.
    /// Drops the scheme prefix — only useful for plain-semver
    /// callers that wouldn't have ended up with a non-`None`
    /// prefix anyway. Callers that care about the prefix should
    /// use [`PkgVerPeer::prefix`] instead.
    pub fn into_tuple(self) -> (Version, String) {
        let PkgVerPeer { prefix: _, version, peer } = self;
        (version, peer)
    }
}

/// Error when parsing [`PkgVerPeer`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParsePkgVerPeerError {
    #[display("Failed to parse the version part: {_0}")]
    ParseVersionFailure(#[error(source)] SemverError),
    #[display("Mismatch parenthesis")]
    MismatchParenthesis,
}

impl FromStr for PkgVerPeer {
    type Err = ParsePkgVerPeerError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Strip an optional `runtime:` prefix first. Only the
        // literal `runtime:` is recognised today — other URL-style
        // schemes (e.g. `tag:`) are out of scope per #511.
        let (prefix, rest) = match value.strip_prefix(RUNTIME_PREFIX) {
            Some(rest) => (Some(RUNTIME_PREFIX.to_string()), rest),
            None => (None, value),
        };

        if !rest.ends_with(')') {
            if rest.find(['(', ')']).is_some() {
                return Err(ParsePkgVerPeerError::MismatchParenthesis);
            }

            let version = rest.parse().map_err(ParsePkgVerPeerError::ParseVersionFailure)?;
            return Ok(PkgVerPeer { prefix, version, peer: String::new() });
        }

        let opening_parenthesis =
            rest.find('(').ok_or(ParsePkgVerPeerError::MismatchParenthesis)?;
        let version = rest[..opening_parenthesis]
            .parse()
            .map_err(ParsePkgVerPeerError::ParseVersionFailure)?;
        let peer = rest[opening_parenthesis..].to_string();
        Ok(PkgVerPeer { prefix, version, peer })
    }
}

impl<'a> TryFrom<Cow<'a, str>> for PkgVerPeer {
    type Error = ParsePkgVerPeerError;
    fn try_from(value: Cow<'a, str>) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<PkgVerPeer> for String {
    fn from(value: PkgVerPeer) -> Self {
        value.to_string()
    }
}

#[cfg(test)]
mod tests;
