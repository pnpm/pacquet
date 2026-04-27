use crate::{ParsePkgNameSuffixError, ParsePkgVerPeerError, PkgName, PkgNameVerPeer, PkgVerPeer};
use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Value of a single entry in [`SnapshotEntry::dependencies`](crate::SnapshotEntry::dependencies)
/// (or `optional_dependencies`).
///
/// A snapshot dependency can be written in one of two forms:
///
/// * A bare version with an optional peer-dependency suffix — the dependency
///   resolves to `<alias-name>@<version>` in the `snapshots:` map.
///
///   ```yaml
///   '@isaacs/cliui@8.0.2':
///     dependencies:
///       string-width: 5.1.2
///   ```
///
/// * An npm-alias of the form `<target-name>@<version>` — the dependency
///   resolves to `<target-name>@<version>` in the `snapshots:` map and the
///   entry key is only used as the directory name inside `node_modules`.
///
///   ```yaml
///   '@isaacs/cliui@8.0.2':
///     dependencies:
///       string-width-cjs: string-width@4.2.3
///   ```
///
/// Detection mirrors pnpm's `refToRelative`: a reference is an alias when a
/// package name appears before the version separator (either the first `@`
/// occurs before any `(` and `:`, or the reference begins with `@`).
///
/// Reference: <https://github.com/pnpm/pnpm/blob/1819226b51/deps/path/src/index.ts>
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "&'de str", into = "String")]
pub enum SnapshotDepRef {
    Plain(PkgVerPeer),
    Alias(PkgNameVerPeer),
}

impl SnapshotDepRef {
    /// Resolve this reference to the `snapshots:` / `packages:` key it points
    /// to. `alias_name` is the key of the dependency entry (the name under
    /// which the package is linked into `node_modules`).
    pub fn resolve(&self, alias_name: &PkgName) -> PkgNameVerPeer {
        match self {
            SnapshotDepRef::Plain(ver_peer) => {
                PkgNameVerPeer::new(alias_name.clone(), ver_peer.clone())
            }
            SnapshotDepRef::Alias(key) => key.clone(),
        }
    }

    /// Accessor for the version-with-peer part of this reference.
    pub fn ver_peer(&self) -> &'_ PkgVerPeer {
        match self {
            SnapshotDepRef::Plain(ver_peer) => ver_peer,
            SnapshotDepRef::Alias(key) => &key.suffix,
        }
    }
}

impl std::fmt::Display for SnapshotDepRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotDepRef::Plain(ver_peer) => ver_peer.fmt(f),
            SnapshotDepRef::Alias(key) => key.fmt(f),
        }
    }
}

/// Returns `true` if `value` looks like an npm-alias reference (i.e. contains
/// a package name before the version separator). See [`SnapshotDepRef`] for
/// the exact rules.
fn looks_like_alias(value: &str) -> bool {
    if value.starts_with('@') {
        return true;
    }
    let Some(at_idx) = value.find('@') else {
        return false;
    };
    let before_paren = value.find('(').is_none_or(|idx| at_idx < idx);
    let before_colon = value.find(':').is_none_or(|idx| at_idx < idx);
    before_paren && before_colon
}

/// Error when parsing [`SnapshotDepRef`] from a string.
#[derive(Debug, Display, Error)]
pub enum ParseSnapshotDepRefError {
    #[display("{_0}")]
    ParsePlain(#[error(source)] ParsePkgVerPeerError),
    #[display("{_0}")]
    ParseAlias(#[error(source)] ParsePkgNameSuffixError<ParsePkgVerPeerError>),
}

impl FromStr for SnapshotDepRef {
    type Err = ParseSnapshotDepRefError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if looks_like_alias(value) {
            let key =
                value.parse::<PkgNameVerPeer>().map_err(ParseSnapshotDepRefError::ParseAlias)?;
            Ok(SnapshotDepRef::Alias(key))
        } else {
            let ver_peer =
                value.parse::<PkgVerPeer>().map_err(ParseSnapshotDepRefError::ParsePlain)?;
            Ok(SnapshotDepRef::Plain(ver_peer))
        }
    }
}

impl<'a> TryFrom<&'a str> for SnapshotDepRef {
    type Error = ParseSnapshotDepRefError;
    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SnapshotDepRef> for String {
    fn from(value: SnapshotDepRef) -> Self {
        value.to_string()
    }
}

impl From<PkgVerPeer> for SnapshotDepRef {
    fn from(value: PkgVerPeer) -> Self {
        SnapshotDepRef::Plain(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn pkg_name(s: &str) -> PkgName {
        PkgName::parse(s).unwrap()
    }

    fn ver_peer(s: &str) -> PkgVerPeer {
        s.parse().unwrap()
    }

    fn key(s: &str) -> PkgNameVerPeer {
        s.parse().unwrap()
    }

    #[test]
    fn parse_plain_version() {
        let dep: SnapshotDepRef = "5.1.2".parse().unwrap();
        assert_eq!(dep, SnapshotDepRef::Plain(ver_peer("5.1.2")));
    }

    #[test]
    fn parse_plain_with_peer_suffix() {
        let dep: SnapshotDepRef = "17.0.2(react@17.0.2)".parse().unwrap();
        assert_eq!(dep, SnapshotDepRef::Plain(ver_peer("17.0.2(react@17.0.2)")));
    }

    #[test]
    fn parse_alias_unscoped_target() {
        let dep: SnapshotDepRef = "string-width@4.2.3".parse().unwrap();
        assert_eq!(dep, SnapshotDepRef::Alias(key("string-width@4.2.3")));
    }

    #[test]
    fn parse_alias_scoped_target() {
        let dep: SnapshotDepRef = "@types/react@17.0.49".parse().unwrap();
        assert_eq!(dep, SnapshotDepRef::Alias(key("@types/react@17.0.49")));
    }

    #[test]
    fn parse_alias_with_peer_suffix() {
        let dep: SnapshotDepRef = "react-dom@17.0.2(react@17.0.2)".parse().unwrap();
        assert_eq!(dep, SnapshotDepRef::Alias(key("react-dom@17.0.2(react@17.0.2)")));
    }

    #[test]
    fn resolve_plain_uses_alias_key_as_target_name() {
        let dep: SnapshotDepRef = "5.1.2".parse().unwrap();
        let resolved = dep.resolve(&pkg_name("string-width"));
        assert_eq!(resolved.to_string(), "string-width@5.1.2");
    }

    #[test]
    fn resolve_alias_uses_alias_target_name_not_key() {
        let dep: SnapshotDepRef = "string-width@4.2.3".parse().unwrap();
        let resolved = dep.resolve(&pkg_name("string-width-cjs"));
        assert_eq!(resolved.to_string(), "string-width@4.2.3");
    }

    #[test]
    fn display_roundtrip() {
        for input in [
            "5.1.2",
            "17.0.2(react@17.0.2)",
            "string-width@4.2.3",
            "@types/react@17.0.49",
            "react-dom@17.0.2(react@17.0.2)",
        ] {
            let dep: SnapshotDepRef = input.parse().unwrap();
            assert_eq!(dep.to_string(), input);
        }
    }

    #[test]
    fn deserialize_ok() {
        for (yaml, expected) in [
            ("5.1.2", "5.1.2"),
            ("string-width@4.2.3", "string-width@4.2.3"),
            ("\"17.0.2(react@17.0.2)\"", "17.0.2(react@17.0.2)"),
        ] {
            let dep: SnapshotDepRef = serde_saphyr::from_str(yaml).unwrap();
            assert_eq!(dep.to_string(), expected);
        }
    }

    #[test]
    fn looks_like_alias_rules() {
        assert!(!looks_like_alias("5.1.2"));
        assert!(!looks_like_alias("17.0.2(react@17.0.2)"));
        assert!(looks_like_alias("string-width@4.2.3"));
        assert!(looks_like_alias("@types/react@17.0.49"));
        assert!(looks_like_alias("react-dom@17.0.2(react@17.0.2)"));
        // protocol-like refs are not aliases
        assert!(!looks_like_alias("link:../foo"));
        assert!(!looks_like_alias("workspace:*"));
    }
}
