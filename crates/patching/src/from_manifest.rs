use crate::group::{PatchInput, PatchNonSemverRangeError, group_patched_dependencies};
use crate::hash::{CalcPatchHashError, create_hex_hash_from_file};
use crate::types::PatchGroupRecord;
use derive_more::{Display, Error};
use miette::Diagnostic;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

/// Error reading `pnpm.patchedDependencies` from a root manifest.
#[derive(Debug, Display, Error, Diagnostic)]
pub enum LoadPatchedDependenciesError {
    #[display("Failed to read {}: {source}", path.display())]
    ReadManifest {
        path: PathBuf,
        #[error(source)]
        source: io::Error,
    },
    #[display("Failed to parse {} as JSON: {source}", path.display())]
    ParseManifest {
        path: PathBuf,
        #[error(source)]
        source: serde_json::Error,
    },
    /// `pnpm.patchedDependencies` was present but not an object whose
    /// values are strings (relative or absolute patch file paths).
    #[display(
        "pnpm.patchedDependencies in {} must be an object mapping package keys to patch file paths",
        path.display()
    )]
    InvalidShape { path: PathBuf },

    #[diagnostic(transparent)]
    Hash(#[error(source)] CalcPatchHashError),

    #[diagnostic(transparent)]
    Range(#[error(source)] PatchNonSemverRangeError),
}

impl From<CalcPatchHashError> for LoadPatchedDependenciesError {
    fn from(e: CalcPatchHashError) -> Self {
        Self::Hash(e)
    }
}

impl From<PatchNonSemverRangeError> for LoadPatchedDependenciesError {
    fn from(e: PatchNonSemverRangeError) -> Self {
        Self::Range(e)
    }
}

/// Read `pnpm.patchedDependencies` from `manifest_dir/package.json`,
/// resolve every relative patch file path against `manifest_dir`,
/// compute each file's SHA-256 hex digest, and bucket the entries
/// with [`group_patched_dependencies`].
///
/// Ports upstream's flow at
/// [`installing/deps-installer/src/install/index.ts:468-488`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/src/install/index.ts#L468-L488)
/// — the parts that turn a root manifest into a [`PatchGroupRecord`].
/// Relative-path resolution mirrors upstream's
/// [`getOptionsFromPnpmSettings`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/getOptionsFromRootManifest.ts#L39-L46).
///
/// Returns `Ok(None)` when:
/// - `package.json` is missing (matches pnpm's `safeReadPackageJsonFromDir`),
/// - `package.json` has no `pnpm` field, or
/// - `package.json` has no `pnpm.patchedDependencies` field, or
/// - `pnpm.patchedDependencies` is an empty object.
///
/// Returns `Err(...)` when the manifest is unreadable, malformed,
/// the field has the wrong shape, or any configured patch file
/// cannot be hashed.
pub fn load_patched_dependencies_from_manifest(
    manifest_dir: &Path,
) -> Result<Option<PatchGroupRecord>, LoadPatchedDependenciesError> {
    let manifest_path = manifest_dir.join("package.json");
    let text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(LoadPatchedDependenciesError::ReadManifest { path: manifest_path, source });
        }
    };

    let manifest: Value = serde_json::from_str(&text).map_err(|source| {
        LoadPatchedDependenciesError::ParseManifest { path: manifest_path.clone(), source }
    })?;

    let Some(patched) = manifest.get("pnpm").and_then(|v| v.get("patchedDependencies")) else {
        return Ok(None);
    };
    let Some(obj) = patched.as_object() else {
        return Err(LoadPatchedDependenciesError::InvalidShape { path: manifest_path });
    };
    if obj.is_empty() {
        return Ok(None);
    }

    let mut paths: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (key, value) in obj {
        let Some(rel_or_abs) = value.as_str() else {
            return Err(LoadPatchedDependenciesError::InvalidShape { path: manifest_path });
        };
        let candidate = Path::new(rel_or_abs);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            manifest_dir.join(candidate)
        };
        paths.insert(key.clone(), resolved);
    }

    let mut inputs: Vec<(String, PatchInput)> = Vec::with_capacity(paths.len());
    for (key, path) in paths {
        let hash = create_hex_hash_from_file(&path)?;
        inputs.push((key, PatchInput { hash, patch_file_path: Some(path) }));
    }

    let groups = group_patched_dependencies(inputs)?;
    Ok(Some(groups))
}

#[cfg(test)]
mod tests;
