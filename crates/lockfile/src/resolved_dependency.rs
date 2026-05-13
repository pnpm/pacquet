use crate::{ParsePkgVerPeerError, PkgName, PkgVerPeer};
use derive_more::{Display, Error};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::{self, Display},
    str::FromStr,
};

/// Map of resolved dependencies stored in a [`ProjectSnapshot`](crate::ProjectSnapshot).
///
/// The keys are package names.
pub type ResolvedDependencyMap = HashMap<PkgName, ResolvedDependencySpec>;

/// Value type of [`ResolvedDependencyMap`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResolvedDependencySpec {
    pub specifier: String,
    pub version: ImporterDepVersion,
}

/// Resolved `version` of an importer-level dependency.
///
/// Importer dependencies (the values inside `importers.<id>.dependencies`
/// in a pnpm v9 lockfile) carry one of two shapes for `version:`:
///
/// - A bare semver-with-peer string like `4.0.0` or `17.0.2(react@17.0.2)`,
///   meaning the dependency is in the shared virtual store.
/// - A `link:<path>` value, meaning the dependency is a workspace
///   sibling at `<path>` relative to the importer's `rootDir`. The
///   workspace project is not duplicated in the virtual store — pnpm
///   creates a direct symlink to the sibling's directory.
///
/// `ImporterDepVersion` encodes the distinction so consumers (the
/// installer, the build-sequence builder, the reporter) can branch on
/// shape without re-parsing the raw string at every call site.
///
/// Snapshot-level dependencies (the values inside `snapshots.*.dependencies`)
/// use [`crate::SnapshotDepRef`] instead, which carries a different
/// distinction (plain vs. alias) and never holds a `link:` value —
/// `link:` only appears at the importer level.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ImporterDepVersion {
    /// Bare semver-with-peer; resolves to a snapshot in `snapshots:`.
    Regular(PkgVerPeer),

    /// `link:<path>` value; resolves to a workspace sibling. The path
    /// is stored verbatim from the lockfile (relative to the
    /// importer's `rootDir`, or absolute) — interpreting it is the
    /// installer's job, not this layer's.
    Link(String),
}

impl ImporterDepVersion {
    /// `Some(ver)` when this dependency resolves through the virtual
    /// store; `None` when it's a `link:` sibling. Mirrors upstream's
    /// `if (depPath.startsWith('link:'))` checks at the install layer.
    pub fn as_regular(&self) -> Option<&'_ PkgVerPeer> {
        match self {
            ImporterDepVersion::Regular(v) => Some(v),
            ImporterDepVersion::Link(_) => None,
        }
    }

    /// `Some(target)` when this dependency is a `link:` sibling;
    /// `None` when it resolves through the virtual store. The
    /// returned string is the path portion *without* the `link:`
    /// prefix.
    pub fn as_link_target(&self) -> Option<&'_ str> {
        match self {
            ImporterDepVersion::Regular(_) => None,
            ImporterDepVersion::Link(target) => Some(target.as_str()),
        }
    }
}

/// Error when parsing [`ImporterDepVersion`].
#[derive(Debug, Display, Error)]
#[non_exhaustive]
pub enum ParseImporterDepVersionError {
    #[display("Failed to parse importer dependency version {value:?}: {source}")]
    Parse {
        value: String,
        #[error(source)]
        source: ParsePkgVerPeerError,
    },
}

impl FromStr for ImporterDepVersion {
    type Err = ParseImporterDepVersionError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // `link:` keeps the path verbatim; everything else parses as a
        // semver-with-peer. The `link:` discriminator is upstream's
        // own — pnpm itself looks for the literal `link:` prefix at
        // install time (see `installDepsResolve` / `lockfileToDepGraph`).
        if let Some(target) = value.strip_prefix("link:") {
            return Ok(ImporterDepVersion::Link(target.to_string()));
        }
        value.parse::<PkgVerPeer>().map(ImporterDepVersion::Regular).map_err(|source| {
            ParseImporterDepVersionError::Parse { value: value.to_string(), source }
        })
    }
}

impl<'a> TryFrom<Cow<'a, str>> for ImporterDepVersion {
    type Error = ParseImporterDepVersionError;
    fn try_from(value: Cow<'a, str>) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ImporterDepVersion> for String {
    fn from(value: ImporterDepVersion) -> Self {
        match value {
            ImporterDepVersion::Regular(v) => v.to_string(),
            ImporterDepVersion::Link(target) => format!("link:{target}"),
        }
    }
}

impl Serialize for ImporterDepVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ImporterDepVersion::Regular(v) => v.serialize(serializer),
            ImporterDepVersion::Link(target) => {
                let formatted = format!("link:{target}");
                serializer.serialize_str(&formatted)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ImporterDepVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Cow::<'de, str>::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

impl From<PkgVerPeer> for ImporterDepVersion {
    fn from(value: PkgVerPeer) -> Self {
        ImporterDepVersion::Regular(value)
    }
}

impl Display for ImporterDepVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImporterDepVersion::Regular(v) => Display::fmt(v, f),
            ImporterDepVersion::Link(target) => write!(f, "link:{target}"),
        }
    }
}

#[cfg(test)]
mod tests;
