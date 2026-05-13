//! Read `pnpm-workspace.yaml` into a [`WorkspaceManifest`].
//!
//! Port of upstream's
//! [`readWorkspaceManifest`](https://github.com/pnpm/pnpm/blob/94240bc046/workspace/workspace-manifest-reader/src/index.ts).
//!
//! Pacquet already has `pacquet_config::WorkspaceSettings` parsing
//! the file for *settings* (`storeDir`, `registry`, …). That stays the
//! authoritative settings parser; this module is concerned only with
//! the workspace-shape fields (`packages:`, catalogs) that drive
//! project enumeration. Splitting them mirrors upstream's package
//! split: `workspace-manifest-reader` returns the typed shape;
//! settings live elsewhere. (Not a rustdoc link — `pacquet-config`
//! is not a dependency of this crate.)
//!
//! The validation rules mirror upstream's
//! [`validateWorkspaceManifest`](https://github.com/pnpm/pnpm/blob/94240bc046/workspace/workspace-manifest-reader/src/index.ts):
//!
//! - File missing → `None`.
//! - Empty / `{}` document → `Some(default)`.
//! - Root is not an object (e.g. a YAML sequence) → error.
//! - `packages` present but not a string array → error.
//! - Empty-string entries in `packages` → error.

use derive_more::{Display, Error};
use miette::Diagnostic;
use serde::Deserialize;
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Basename of the workspace manifest. Same constant pnpm uses
/// internally via `@pnpm/constants`.
pub const WORKSPACE_MANIFEST_FILENAME: &str = "pnpm-workspace.yaml";

/// Subset of `pnpm-workspace.yaml` consumed by project enumeration.
///
/// The settings half (`storeDir`, `registry`, lifecycle policies, …)
/// is read separately by `pacquet_config::WorkspaceSettings`.
/// Splitting along the upstream package boundary keeps each reader
/// focused on the shape its callers actually need and avoids a
/// monolithic struct that has to grow with every new pnpm setting.
///
/// Catalogs are accepted at parse time but not yet consumed downstream
/// — they belong to Stage 2 of the workspace roadmap. Parsing them now
/// keeps `pnpm-workspace.yaml` files with catalog entries valid input
/// to pacquet rather than tripping `deny_unknown_fields`.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceManifest {
    /// Glob patterns identifying the workspace's projects, relative to
    /// the workspace dir. Empty when omitted; an explicit empty list is
    /// distinguishable from "omitted" because an empty document yields
    /// the default (empty) value too.
    #[serde(default)]
    pub packages: Vec<String>,
}

/// Raised when `pnpm-workspace.yaml` parses as YAML but fails an
/// upstream-mirrored shape check that serde itself can't enforce.
/// Same error code as upstream's `InvalidWorkspaceManifestError`.
///
/// Note: upstream's "packages field is not an array" branch is
/// covered by [`ReadWorkspaceManifestError::ParseYaml`] in pacquet —
/// `serde_saphyr` rejects a non-array shape before this layer runs.
/// Only the empty-string-entry check needs a dedicated variant.
#[derive(Debug, Display, Error, Diagnostic)]
#[diagnostic(code(pacquet_workspace::invalid_workspace_configuration))]
#[non_exhaustive]
pub enum InvalidWorkspaceManifestError {
    #[display("Missing or empty package")]
    EmptyPackageEntry,
}

/// Error type of [`read_workspace_manifest`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum ReadWorkspaceManifestError {
    #[display("Failed to read pnpm-workspace.yaml at {}: {source}", path.display())]
    ReadFile {
        path: PathBuf,
        #[error(source)]
        source: io::Error,
    },
    #[display("Failed to parse pnpm-workspace.yaml at {}: {source}", path.display())]
    ParseYaml {
        path: PathBuf,
        #[error(source)]
        source: Box<serde_saphyr::Error>,
    },
    #[diagnostic(transparent)]
    Invalid(#[error(source)] InvalidWorkspaceManifestError),
}

/// Read and validate the `pnpm-workspace.yaml` under `dir`.
///
/// Returns `Ok(None)` when the file does not exist (matching upstream's
/// `ENOENT`-as-undefined contract). Every other read or parse failure
/// propagates.
pub fn read_workspace_manifest(
    dir: &Path,
) -> Result<Option<WorkspaceManifest>, ReadWorkspaceManifestError> {
    let path = dir.join(WORKSPACE_MANIFEST_FILENAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ReadWorkspaceManifestError::ReadFile { path, source }),
    };

    // An empty workspace manifest is valid and means "no settings, no
    // packages" — same as `{}`. `serde_saphyr` would otherwise reject
    // an empty document; short-circuit to the default value to match
    // upstream's behavior.
    if text.trim().is_empty() {
        return Ok(Some(WorkspaceManifest::default()));
    }

    let manifest: WorkspaceManifest = serde_saphyr::from_str(&text).map_err(|source| {
        ReadWorkspaceManifestError::ParseYaml { path: path.clone(), source: Box::new(source) }
    })?;

    // serde_saphyr already enforces the array shape and string type
    // for `packages:` at deserialization. The remaining upstream
    // invariant — entries cannot be empty strings — needs a manual
    // pass since serde doesn't know about that constraint.
    for entry in &manifest.packages {
        if entry.is_empty() {
            return Err(ReadWorkspaceManifestError::Invalid(
                InvalidWorkspaceManifestError::EmptyPackageEntry,
            ));
        }
    }

    Ok(Some(manifest))
}

#[cfg(test)]
mod tests;
