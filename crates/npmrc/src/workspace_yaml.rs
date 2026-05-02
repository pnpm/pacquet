use crate::{NodeLinker, Npmrc, PackageImportMethod};
use pacquet_store_dir::StoreDir;
use serde::Deserialize;
use std::{
    fs, io,
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
#[derive(Debug)]
#[non_exhaustive]
pub enum LoadWorkspaceYamlError {
    ReadFile(io::Error),
    ParseYaml(serde_saphyr::Error),
}

impl WorkspaceSettings {
    /// Walk up from `start_dir` looking for a `pnpm-workspace.yaml`. Returns
    /// `Ok(None)` if none is found before reaching the filesystem root.
    ///
    /// Mirrors pnpm's behaviour in
    /// [`loadNpmrcFiles.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/config/reader/src/loadNpmrcFiles.ts)
    /// — the first ancestor containing a `pnpm-workspace.yaml` is the
    /// workspace root, and its config applies.
    pub fn find_and_load(
        start_dir: &Path,
    ) -> Result<Option<(PathBuf, Self)>, LoadWorkspaceYamlError> {
        let Some(path) = find_workspace_manifest(start_dir) else {
            return Ok(None);
        };
        let text = fs::read_to_string(&path).map_err(LoadWorkspaceYamlError::ReadFile)?;
        let settings: WorkspaceSettings =
            serde_saphyr::from_str(&text).map_err(LoadWorkspaceYamlError::ParseYaml)?;
        Ok(Some((path, settings)))
    }

    /// Apply every set field onto `npmrc`, leaving unset ones untouched.
    ///
    /// Path-valued fields (`store_dir`, `modules_dir`, `virtual_store_dir`)
    /// are resolved against `base_dir` if relative — mirroring `.npmrc`'s
    /// resolve-against-cwd behaviour but anchored at the workspace root
    /// where the yaml was found, which is what pnpm does.
    pub fn apply_to(self, npmrc: &mut Npmrc, base_dir: &Path) {
        if let Some(v) = self.hoist {
            npmrc.hoist = v;
        }
        if let Some(v) = self.hoist_pattern {
            npmrc.hoist_pattern = v;
        }
        if let Some(v) = self.public_hoist_pattern {
            npmrc.public_hoist_pattern = v;
        }
        if let Some(v) = self.shamefully_hoist {
            npmrc.shamefully_hoist = v;
        }
        if let Some(v) = self.store_dir {
            npmrc.store_dir = StoreDir::from(resolve(base_dir, &v));
        }
        if let Some(v) = self.modules_dir {
            npmrc.modules_dir = resolve(base_dir, &v);
        }
        if let Some(v) = self.node_linker {
            npmrc.node_linker = v;
        }
        if let Some(v) = self.symlink {
            npmrc.symlink = v;
        }
        if let Some(v) = self.virtual_store_dir {
            npmrc.virtual_store_dir = resolve(base_dir, &v);
        }
        if let Some(v) = self.package_import_method {
            npmrc.package_import_method = v;
        }
        if let Some(v) = self.modules_cache_max_age {
            npmrc.modules_cache_max_age = v;
        }
        if let Some(v) = self.lockfile {
            npmrc.lockfile = v;
        }
        if let Some(v) = self.prefer_frozen_lockfile {
            npmrc.prefer_frozen_lockfile = v;
        }
        if let Some(v) = self.lockfile_include_tarball_url {
            npmrc.lockfile_include_tarball_url = v;
        }
        if let Some(v) = self.registry {
            npmrc.registry = if v.ends_with('/') { v } else { format!("{v}/") };
        }
        if let Some(v) = self.auto_install_peers {
            npmrc.auto_install_peers = v;
        }
        if let Some(v) = self.dedupe_peer_dependents {
            npmrc.dedupe_peer_dependents = v;
        }
        if let Some(v) = self.strict_peer_dependencies {
            npmrc.strict_peer_dependencies = v;
        }
        if let Some(v) = self.resolve_peers_from_workspace_root {
            npmrc.resolve_peers_from_workspace_root = v;
        }
        if let Some(v) = self.verify_store_integrity {
            npmrc.verify_store_integrity = v;
        }
        if let Some(v) = self.fetch_retries {
            npmrc.fetch_retries = v;
        }
        if let Some(v) = self.fetch_retry_factor {
            npmrc.fetch_retry_factor = v;
        }
        if let Some(v) = self.fetch_retry_mintimeout {
            npmrc.fetch_retry_mintimeout = v;
        }
        if let Some(v) = self.fetch_retry_maxtimeout {
            npmrc.fetch_retry_maxtimeout = v;
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
