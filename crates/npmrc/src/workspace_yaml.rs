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
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
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
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_common_settings_from_yaml() {
        let yaml = r#"
storeDir: ../my-store
registry: https://reg.example
lockfile: false
autoInstallPeers: true
nodeLinker: hoisted
packages:
  - packages/*
"#;
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(settings.store_dir.as_deref(), Some("../my-store"));
        assert_eq!(settings.registry.as_deref(), Some("https://reg.example"));
        assert_eq!(settings.lockfile, Some(false));
        assert_eq!(settings.auto_install_peers, Some(true));
        assert!(matches!(settings.node_linker, Some(NodeLinker::Hoisted)));
    }

    #[test]
    fn swallows_unknown_top_level_keys() {
        let yaml = r#"
catalog:
  react: ^18
onlyBuiltDependencies:
  - esbuild
packages:
  - "apps/*"
"#;
        // `pnpm-workspace.yaml` commonly contains top-level keys we do not
        // model in `WorkspaceSettings` (packages list, catalogs, build
        // allow-lists, …). This guards against regressions that would make
        // serde reject those unknown keys during deserialization — i.e.
        // someone adding `deny_unknown_fields` to the struct.
        let _settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
    }

    #[test]
    fn apply_overrides_npmrc_defaults() {
        let yaml = r#"
storeDir: /absolute/store
lockfile: false
registry: https://reg.example
"#;
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        let mut npmrc = Npmrc::new();
        npmrc.lockfile = true;
        let before_registry = npmrc.registry.clone();

        settings.apply_to(&mut npmrc, Path::new("/irrelevant-for-absolute-paths"));

        assert_eq!(npmrc.store_dir.display().to_string(), "/absolute/store");
        assert!(!npmrc.lockfile);
        assert_eq!(npmrc.registry, "https://reg.example/");
        assert_ne!(before_registry, npmrc.registry);
    }

    #[test]
    fn apply_resolves_relative_paths_against_base_dir() {
        let yaml = "storeDir: ../shared-store\n";
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        let mut npmrc = Npmrc::new();
        let base = Path::new("/workspace/root");

        settings.apply_to(&mut npmrc, base);

        // Build the expected path via the same join machinery the code
        // under test uses so the component separator matches on every
        // platform (Windows uses `\` between joined components).
        assert_eq!(npmrc.store_dir, StoreDir::from(base.join("../shared-store")));
    }

    /// pnpm reads `fetchRetries` / `fetchRetryFactor` /
    /// `fetchRetryMintimeout` / `fetchRetryMaxtimeout` from
    /// `pnpm-workspace.yaml` as camelCase keys (mirrors of the kebab-case
    /// `.npmrc` form). Confirm both deserialization and `apply_to` push
    /// the overrides onto the `Npmrc`, since pacquet has to honour them
    /// for parity with pnpm and for the install-time retry plumbing in
    /// crates/tarball.
    #[test]
    fn parses_fetch_retry_settings_from_yaml_and_applies() {
        let yaml = r#"
fetchRetries: 5
fetchRetryFactor: 3
fetchRetryMintimeout: 1000
fetchRetryMaxtimeout: 4000
"#;
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(settings.fetch_retries, Some(5));
        assert_eq!(settings.fetch_retry_factor, Some(3));
        assert_eq!(settings.fetch_retry_mintimeout, Some(1000));
        assert_eq!(settings.fetch_retry_maxtimeout, Some(4000));

        let mut npmrc = Npmrc::new();
        settings.apply_to(&mut npmrc, Path::new("/irrelevant"));
        assert_eq!(npmrc.fetch_retries, 5);
        assert_eq!(npmrc.fetch_retry_factor, 3);
        assert_eq!(npmrc.fetch_retry_mintimeout, 1000);
        assert_eq!(npmrc.fetch_retry_maxtimeout, 4000);
    }

    /// `verifyStoreIntegrity` is a camelCase key that serde's rename
    /// has to pick up, and the `apply_to` wiring has to thread it onto
    /// the `Npmrc` field. Parse a yaml that flips the default-true
    /// setting to false and assert both steps. Guards against silent
    /// regressions in the key mapping or the apply step (a copy-paste
    /// omission in `apply_to` would leave `npmrc.verify_store_integrity`
    /// at its default).
    #[test]
    fn parses_verify_store_integrity_from_yaml_and_applies() {
        let yaml = "verifyStoreIntegrity: false\n";
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(settings.verify_store_integrity, Some(false));

        let mut npmrc = Npmrc::new();
        assert!(npmrc.verify_store_integrity, "the default is `true` to match pnpm");
        settings.apply_to(&mut npmrc, Path::new("/irrelevant"));
        assert!(!npmrc.verify_store_integrity, "yaml override wins");
    }

    #[test]
    fn apply_leaves_unset_fields_alone() {
        let yaml = "storeDir: /s\n";
        let settings: WorkspaceSettings = serde_saphyr::from_str(yaml).unwrap();
        let mut npmrc = Npmrc::new();
        let before =
            (npmrc.hoist, npmrc.lockfile, npmrc.registry.clone(), npmrc.auto_install_peers);

        settings.apply_to(&mut npmrc, Path::new("/anywhere"));

        assert_eq!(
            (npmrc.hoist, npmrc.lockfile, npmrc.registry.clone(), npmrc.auto_install_peers),
            before
        );
    }

    #[test]
    fn find_walks_up_to_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(tmp.path().join("pnpm-workspace.yaml"), "storeDir: /s\n").unwrap();

        let (found, settings) = WorkspaceSettings::find_and_load(&nested).unwrap().unwrap();
        assert_eq!(found, tmp.path().join("pnpm-workspace.yaml"));
        assert_eq!(settings.store_dir.as_deref(), Some("/s"));
    }

    #[test]
    fn find_returns_none_when_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(WorkspaceSettings::find_and_load(tmp.path()).unwrap().is_none());
    }
}
