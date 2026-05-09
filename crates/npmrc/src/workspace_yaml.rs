use crate::{NodeLinker, Npmrc, PackageImportMethod};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_store_dir::StoreDir;
use pipe_trait::Pipe;
use serde::Deserialize;
use std::{
    fs,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
};

/// Settings readable from `pnpm-workspace.yaml`.
///
/// pnpm 10+ moved the bulk of its configuration (`storeDir`, `registry`,
/// `lockfile`, …) out of `.npmrc` into `pnpm-workspace.yaml`, using
/// camelCase keys. Pacquet needs to honour these overrides so a real
/// pnpm-11-style project — where `.npmrc` may not even contain the
/// settings — works out of the box.
///
/// Every field is `Option` because the yaml is strictly additive on top of
/// [`Npmrc`]: anything left unset falls through to whatever `.npmrc` provided
/// (or the hard-coded default).
///
/// See <https://pnpm.io/settings> for the canonical key list.
/// Non-config keys in a real pnpm-workspace.yaml (`packages`, `catalog`,
/// `catalogs`, `onlyBuiltDependencies`, `allowBuilds`, …) are silently
/// ignored — serde drops them since the struct doesn't use
/// `deny_unknown_fields`.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceSettings {
    pub hoist: Option<bool>,
    pub hoist_pattern: Option<Vec<String>>,
    pub public_hoist_pattern: Option<Vec<String>>,
    pub shamefully_hoist: Option<bool>,
    pub store_dir: Option<String>,
    pub modules_dir: Option<String>,
    pub node_linker: Option<NodeLinker>,
    pub symlink: Option<bool>,
    pub virtual_store_dir: Option<String>,
    pub package_import_method: Option<PackageImportMethod>,
    pub modules_cache_max_age: Option<u64>,
    pub lockfile: Option<bool>,
    pub prefer_frozen_lockfile: Option<bool>,
    pub lockfile_include_tarball_url: Option<bool>,
    pub registry: Option<String>,
    pub auto_install_peers: Option<bool>,
    pub dedupe_peer_dependents: Option<bool>,
    pub strict_peer_dependencies: Option<bool>,
    pub resolve_peers_from_workspace_root: Option<bool>,
    pub verify_store_integrity: Option<bool>,
    pub fetch_retries: Option<u32>,
    pub fetch_retry_factor: Option<u32>,
    pub fetch_retry_mintimeout: Option<u64>,
    pub fetch_retry_maxtimeout: Option<u64>,
}

/// Basename of the file pnpm reads; exported for test use.
pub const WORKSPACE_MANIFEST_FILENAME: &str = "pnpm-workspace.yaml";

/// Error when reading `pnpm-workspace.yaml`.
///
/// Pnpm's
/// [`workspace-manifest-reader`](https://github.com/pnpm/pnpm/blob/8eb1be4988/workspace/workspace-manifest-reader/src/index.ts)
/// treats `ENOENT` as "no manifest" and propagates every other failure.
/// Pacquet mirrors that split. `serde_saphyr::Error` is boxed so the
/// returned `Result` stays small.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum LoadWorkspaceYamlError {
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
}

impl WorkspaceSettings {
    /// Walk up from `start_dir` looking for a readable `pnpm-workspace.yaml`.
    /// Returns `Ok(None)` if no ancestor has one. Read or parse failures
    /// other than `ENOENT` propagate, matching pnpm's
    /// [`readManifestRaw`](https://github.com/pnpm/pnpm/blob/8eb1be4988/workspace/workspace-manifest-reader/src/index.ts).
    pub fn find_and_load(
        start_dir: &Path,
    ) -> Result<Option<(PathBuf, Self)>, LoadWorkspaceYamlError> {
        for dir in start_dir.ancestors() {
            let path = dir.join(WORKSPACE_MANIFEST_FILENAME);
            let read_result = fs::read_to_string(&path);

            // Walk up only when the read failed because nothing exists at
            // this level. Every other error (including `EISDIR` for a
            // directory named `pnpm-workspace.yaml`, or permission denied)
            // propagates, matching pnpm where `ENOENT` is the only silent
            // case.
            if let Err(error) = &read_result
                && error.kind() == ErrorKind::NotFound
            {
                continue;
            }

            let settings: WorkspaceSettings = read_result
                .map_err(|source| LoadWorkspaceYamlError::ReadFile { path: path.clone(), source })?
                .pipe_as_ref(serde_saphyr::from_str)
                .map_err(Box::new)
                .map_err(|source| LoadWorkspaceYamlError::ParseYaml {
                    path: path.clone(),
                    source,
                })?;

            return Ok(Some((path, settings)));
        }

        Ok(None)
    }

    /// Apply every set field onto `npmrc`, leaving unset ones untouched.
    ///
    /// Path-valued fields (`store_dir`, `modules_dir`, `virtual_store_dir`)
    /// are resolved against `base_dir` if relative — mirroring `.npmrc`'s
    /// resolve-against-cwd behaviour but anchored at the workspace root
    /// where the yaml was found, which is what pnpm does.
    pub fn apply_to(self, npmrc: &mut Npmrc, base_dir: &Path) {
        macro_rules! apply {
            ($($field:ident),* $(,)?) => {$(
                if let Some(v) = self.$field {
                    npmrc.$field = v;
                }
            )*};
        }

        apply! {
            hoist, hoist_pattern, public_hoist_pattern, shamefully_hoist,
            node_linker, symlink, package_import_method, modules_cache_max_age,
            lockfile, prefer_frozen_lockfile, lockfile_include_tarball_url,
            auto_install_peers, dedupe_peer_dependents, strict_peer_dependencies,
            resolve_peers_from_workspace_root, verify_store_integrity,
            fetch_retries, fetch_retry_factor, fetch_retry_mintimeout,
            fetch_retry_maxtimeout,
        }

        if let Some(v) = self.modules_dir {
            npmrc.modules_dir = resolve(base_dir, &v);
        }
        if let Some(v) = self.virtual_store_dir {
            npmrc.virtual_store_dir = resolve(base_dir, &v);
        }
        if let Some(v) = self.store_dir {
            npmrc.store_dir = StoreDir::from(resolve(base_dir, &v));
        }
        if let Some(v) = self.registry {
            npmrc.registry = if v.ends_with('/') { v } else { format!("{v}/") };
        }
    }
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() { candidate.to_path_buf() } else { base.join(candidate) }
}

fn find_workspace_manifest(start: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start);
    while let Some(dir) = cursor {
        let candidate = dir.join(WORKSPACE_MANIFEST_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

/// Resolve the workspace root for a given starting directory — i.e. the
/// directory containing the nearest ancestor `pnpm-workspace.yaml`.
/// Returns `start` itself if no manifest is found, so callers can always
/// use the result as a resolution base.
pub fn workspace_root_or(start: &Path) -> PathBuf {
    find_workspace_manifest(start)
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| start.to_path_buf())
}

#[cfg(test)]
mod tests;
