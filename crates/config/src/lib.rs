mod defaults;
mod npmrc_auth;
#[cfg(test)]
mod test_env_guard;
mod workspace_yaml;

use indexmap::IndexMap;
use pacquet_patching::{PatchGroupRecord, ResolvePatchedDependenciesError, resolve_and_group};
use pacquet_store_dir::StoreDir;
use pipe_trait::Pipe;
use serde::Deserialize;
use smart_default::SmartDefault;
use std::{collections::HashMap, fs, path::PathBuf};

use crate::defaults::{
    default_fetch_retries, default_fetch_retry_factor, default_fetch_retry_maxtimeout,
    default_fetch_retry_mintimeout, default_hoist_pattern, default_modules_cache_max_age,
    default_modules_dir, default_public_hoist_pattern, default_registry, default_store_dir,
    default_virtual_store_dir,
};
pub use workspace_yaml::{
    LoadWorkspaceYamlError, WORKSPACE_MANIFEST_FILENAME, WorkspaceSettings, workspace_root_or,
};

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum NodeLinker {
    /// dependencies are symlinked from a virtual store at node_modules/.pnpm.
    #[default]
    Isolated,

    /// flat node_modules without symlinks is created. Same as the node_modules created by npm or
    /// Yarn Classic.
    Hoisted,

    /// no node_modules. Plug'n'Play is an innovative strategy for Node that is used by
    /// Yarn Berry. It is recommended to also set symlink setting to false when using pnp as
    /// your linker.
    Pnp,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageImportMethod {
    ///  try to clone packages from the store. If cloning is not supported then hardlink packages
    /// from the store. If neither cloning nor linking is possible, fall back to copying
    #[default]
    Auto,

    /// hard link packages from the store
    Hardlink,

    /// copy packages from the store
    Copy,

    /// clone (AKA copy-on-write or reference link) packages from the store
    Clone,

    /// try to clone packages from the store. If cloning is not supported then fall back to copying
    CloneOrCopy,
}

/// Resolved runtime config built from defaults, the auth subset of
/// `.npmrc`, and `pnpm-workspace.yaml` (see [`Config::current`]).
///
/// The type carries the merged result — it is never deserialized from a
/// file directly. Yaml is parsed into [`WorkspaceSettings`] and applied
/// onto `Config` field-by-field, mirroring pnpm 11's split between
/// `.npmrc` (auth/registry/network) and `pnpm-workspace.yaml`
/// (project-structural settings).
#[derive(Debug, SmartDefault)]
pub struct Config {
    /// When true, all dependencies are hoisted to node_modules/.pnpm/node_modules.
    /// This makes unlisted dependencies accessible to all packages inside node_modules.
    #[default = true]
    pub hoist: bool,

    /// Tells pnpm which packages should be hoisted to node_modules/.pnpm/node_modules.
    /// By default, all packages are hoisted - however, if you know that only some flawed packages
    /// have phantom dependencies, you can use this option to exclusively hoist the phantom
    /// dependencies (recommended).
    #[default(_code = "default_hoist_pattern()")]
    pub hoist_pattern: Vec<String>,

    /// Unlike hoist-pattern, which hoists dependencies to a hidden modules directory inside the
    /// virtual store, public-hoist-pattern hoists dependencies matching the pattern to the root
    /// modules directory. Hoisting to the root modules directory means that application code will
    /// have access to phantom dependencies, even if they modify the resolution strategy improperly.
    #[default(_code = "default_public_hoist_pattern()")]
    pub public_hoist_pattern: Vec<String>,

    /// By default, pnpm creates a semistrict node_modules, meaning dependencies have access to
    /// undeclared dependencies but modules outside of node_modules do not. With this layout,
    /// most of the packages in the ecosystem work with no issues. However, if some tooling only
    /// works when the hoisted dependencies are in the root of node_modules, you can set this to
    /// true to hoist them for you.
    pub shamefully_hoist: bool,

    /// The location where all the packages are saved on the disk.
    #[default(_code = "default_store_dir()")]
    pub store_dir: StoreDir,

    /// The directory in which dependencies will be installed (instead of node_modules).
    #[default(_code = "default_modules_dir()")]
    pub modules_dir: PathBuf,

    /// Defines what linker should be used for installing Node packages.
    pub node_linker: NodeLinker,

    /// When symlink is set to false, pnpm creates a virtual store directory without any symlinks.
    /// It is a useful setting together with node-linker=pnp.
    #[default = true]
    pub symlink: bool,

    /// The directory with links to the store. All direct and indirect dependencies of the
    /// project are linked into this directory.
    #[default(_code = "default_virtual_store_dir()")]
    pub virtual_store_dir: PathBuf,

    /// Controls the way packages are imported from the store (if you want to disable symlinks
    /// inside node_modules, then you need to change the node-linker setting, not this one).
    pub package_import_method: PackageImportMethod,

    /// The time in minutes after which orphan packages from the modules directory should be
    /// removed. pnpm keeps a cache of packages in the modules directory. This boosts installation
    /// speed when switching branches or downgrading dependencies.
    ///
    /// Default value is 10080 (7 days in minutes)
    #[default(_code = "default_modules_cache_max_age()")]
    pub modules_cache_max_age: u64,

    /// When set to false, pnpm won't read or generate a pnpm-lock.yaml file.
    pub lockfile: bool,

    /// When set to true and the available pnpm-lock.yaml satisfies the package.json dependencies
    /// directive, a headless installation is performed. A headless installation skips all
    /// dependency resolution as it does not need to modify the lockfile.
    #[default = true]
    pub prefer_frozen_lockfile: bool,

    /// Add the full URL to the package's tarball to every entry in pnpm-lock.yaml.
    pub lockfile_include_tarball_url: bool,

    /// The base URL of the npm package registry (trailing slash included).
    #[default(_code = "default_registry()")]
    pub registry: String, // TODO: use Url type (compatible with reqwest)

    /// When true, any missing non-optional peer dependencies are automatically installed.
    #[default = true]
    pub auto_install_peers: bool,

    /// When this setting is set to true, packages with peer dependencies will be deduplicated after peers resolution.
    #[default = true]
    pub dedupe_peer_dependents: bool,

    /// If this is enabled, commands will fail if there is a missing or invalid peer dependency in the tree.
    pub strict_peer_dependencies: bool,

    /// When enabled, dependencies of the root workspace project are used to resolve peer
    /// dependencies of any projects in the workspace. It is a useful feature as you can install
    /// your peer dependencies only in the root of the workspace, and you can be sure that all
    /// projects in the workspace use the same versions of the peer dependencies.
    #[default = true]
    pub resolve_peers_from_workspace_root: bool,

    /// Whether to verify each CAFS file's on-disk integrity before reusing it
    /// for an install. When `true` (pnpm's default), the store-index cache
    /// lookup stats each referenced file and re-hashes any whose mtime has
    /// advanced past the stored `checkedAt` timestamp. When `false`, the
    /// lookup skips that verification entirely and trusts the index — a
    /// missing blob is discovered lazily at link time instead.
    ///
    /// Matches pnpm's `verifyStoreIntegrity` camelCase key in
    /// `pnpm-workspace.yaml` (same `true` default as pnpm's
    /// `installing/deps-installer/src/install/extendInstallOptions.ts`).
    #[default = true]
    pub verify_store_integrity: bool,

    /// Whether to consult the side-effects cache
    /// (`PackageFilesIndex.sideEffects`) when importing a package
    /// and whether to populate it after a successful postinstall.
    /// Read from `pnpm-workspace.yaml`'s `sideEffectsCache` field
    /// (camelCase, optional, defaults `true`).
    ///
    /// Default `true`, matching pnpm's `side-effects-cache` at
    /// [`config/reader/src/index.ts`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/index.ts#L614-L615).
    ///
    /// The READ gate combines this with [`side_effects_cache_readonly`]
    /// via [`Config::side_effects_cache_read`]; the WRITE gate via
    /// [`Config::side_effects_cache_write`]. Consume those helpers
    /// rather than reading this field directly so the precedence
    /// stays single-sourced.
    ///
    /// [`side_effects_cache_readonly`]: Self::side_effects_cache_readonly
    #[default = true]
    pub side_effects_cache: bool,

    /// Treat the side-effects cache as read-only — pacquet still
    /// honors cache hits on the READ side but does not populate
    /// the cache after a successful postinstall. Mirrors pnpm's
    /// [`side-effects-cache-readonly`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/Config.ts#L124).
    /// Default `false`. Read from `pnpm-workspace.yaml`'s
    /// `sideEffectsCacheReadonly` field.
    ///
    /// Consume via [`Config::side_effects_cache_read`] and
    /// [`Config::side_effects_cache_write`].
    pub side_effects_cache_readonly: bool,

    /// How many times pacquet retries a failed tarball fetch on transient
    /// errors before giving up. Mirrors pnpm's `fetchRetries` (default
    /// `2`, matching `config/config/src/index.ts`). The value is the count
    /// of *retries*, so total attempts = `fetch_retries + 1`.
    ///
    /// Today this only gates the `pacquet-tarball` download path;
    /// `crates/registry`'s metadata fetches still issue a single request.
    /// Threading the same retry policy through the registry client is a
    /// follow-up.
    ///
    /// Read from `pnpm-workspace.yaml` only — pnpm 11's
    /// [`isIniConfigKey`](https://github.com/pnpm/pnpm/blob/1819226b51/config/reader/src/localConfig.ts#L160-L161)
    /// excludes the `fetch-retry*` family from `NPM_AUTH_SETTINGS`, so a
    /// `fetch-retries=…` line in `.npmrc` is ignored upstream and is
    /// ignored here too.
    #[default(_code = "default_fetch_retries()")]
    pub fetch_retries: u32,

    /// Exponential-backoff growth factor between retry attempts. Mirrors
    /// pnpm's `fetchRetryFactor` (default `10`). Successive backoff is
    /// `min(fetch_retry_mintimeout * factor^attempt, fetch_retry_maxtimeout)`.
    /// Yaml-only — see [`Config::fetch_retries`].
    #[default(_code = "default_fetch_retry_factor()")]
    pub fetch_retry_factor: u32,

    /// Floor in milliseconds for the wait between retries. Mirrors pnpm's
    /// `fetchRetryMintimeout` (default `10000` — 10 s). Yaml-only — see
    /// [`Config::fetch_retries`].
    #[default(_code = "default_fetch_retry_mintimeout()")]
    pub fetch_retry_mintimeout: u64,

    /// Cap in milliseconds on the wait between retries. Mirrors pnpm's
    /// `fetchRetryMaxtimeout` (default `60000` — 1 min). Yaml-only —
    /// see [`Config::fetch_retries`].
    #[default(_code = "default_fetch_retry_maxtimeout()")]
    pub fetch_retry_maxtimeout: u64,

    /// Directory containing the nearest ancestor `pnpm-workspace.yaml`.
    /// Set by [`WorkspaceSettings::apply_to`] when yaml was found, so
    /// later install-time code (notably [`resolve_and_group`] for
    /// `patchedDependencies`) can resolve relative paths against the
    /// same dir pnpm does. `None` when no `pnpm-workspace.yaml` exists
    /// anywhere up the tree — in that case there are no patches /
    /// allowBuilds settings to resolve either.
    pub workspace_dir: Option<PathBuf>,

    /// Raw `patchedDependencies` from `pnpm-workspace.yaml`: keys are
    /// `name[@version]`, values are patch file paths (relative to
    /// `workspace_dir` or absolute). Consumed by
    /// [`Config::resolved_patched_dependencies`] which performs the
    /// path resolution and SHA-256 hashing.
    ///
    /// [`IndexMap`] preserves user-specified order so range entries
    /// land in `PatchGroup.range` in the same order they appear in
    /// yaml — matching upstream's JS-object iteration and keeping
    /// `PATCH_KEY_CONFLICT` diagnostics aligned.
    ///
    /// pnpm v11 reads `patchedDependencies` from `pnpm-workspace.yaml`
    /// only — see upstream's
    /// [`addSettingsFromWorkspaceManifestToConfig`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/index.ts#L803-L831).
    pub patched_dependencies: Option<IndexMap<String, String>>,

    /// `pnpm.allowBuilds` from `pnpm-workspace.yaml`: package names
    /// (or `name@version` keys) that are allowed to run lifecycle
    /// scripts. pnpm 11 denies scripts by default; the allow-list is
    /// the opt-in mechanism. Consumed by `AllowBuildPolicy::from_config`
    /// in `pacquet-package-manager`.
    ///
    /// Default empty. Mirrors upstream's
    /// [`createAllowBuildFunction`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/building/policy/src/index.ts).
    pub allow_builds: HashMap<String, bool>,

    /// `dangerouslyAllowAllBuilds` from `pnpm-workspace.yaml`. When
    /// `true`, every package may run lifecycle scripts regardless of
    /// `allow_builds`. Default `false` to match pnpm v11.
    pub dangerously_allow_all_builds: bool,
}

impl Config {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the install should consult the side-effects cache.
    /// Mirrors upstream's
    /// [`sideEffectsCacheRead = sideEffectsCache ?? sideEffectsCacheReadonly`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/index.ts#L614).
    ///
    /// Pacquet collapses upstream's tri-state (`undefined`/`true`/`false`)
    /// into two booleans: the cache is read when either flag is on, so
    /// users who only want the READ side can set
    /// `sideEffectsCacheReadonly: true` with `sideEffectsCache: false`
    /// and get a read-only view.
    pub fn side_effects_cache_read(&self) -> bool {
        self.side_effects_cache || self.side_effects_cache_readonly
    }

    /// Whether the install is allowed to populate the side-effects
    /// cache after a successful postinstall. Mirrors upstream's
    /// [`sideEffectsCacheWrite = sideEffectsCache`](https://github.com/pnpm/pnpm/blob/7e3145f9fc/config/reader/src/index.ts#L615)
    /// with the additional constraint that the explicit
    /// `sideEffectsCacheReadonly: true` always wins — upstream's
    /// `??` semantics let `readonly` slip through when both flags
    /// are explicitly set, but `readonly` as a flag name only makes
    /// sense if it really does block writes.
    pub fn side_effects_cache_write(&self) -> bool {
        self.side_effects_cache && !self.side_effects_cache_readonly
    }

    /// Resolve relative patch file paths in
    /// [`Config::patched_dependencies`] against
    /// [`Config::workspace_dir`], compute SHA-256 hashes, and bucket
    /// the entries into a [`PatchGroupRecord`].
    ///
    /// Mirrors the workspace-dir half of upstream's
    /// [`getOptionsFromPnpmSettings`](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/config/reader/src/getOptionsFromRootManifest.ts#L28-L46)
    /// composed with the
    /// [`calcPatchHashes` step](https://github.com/pnpm/pnpm/blob/b4f8f47ac2/installing/deps-installer/src/install/index.ts#L468-L488).
    ///
    /// Returns `Ok(None)` when either field is unset (no yaml
    /// found or no `patchedDependencies` key). Returns `Err(_)`
    /// when any patch file can't be hashed or any key has an
    /// invalid semver range.
    ///
    /// IO-heavy; call once per install rather than at every site
    /// that needs the resolved record.
    pub fn resolved_patched_dependencies(
        &self,
    ) -> Result<Option<PatchGroupRecord>, ResolvePatchedDependenciesError> {
        let (Some(workspace_dir), Some(raw)) = (&self.workspace_dir, &self.patched_dependencies)
        else {
            return Ok(None);
        };
        resolve_and_group(workspace_dir, raw)
    }

    /// Build the runtime config by layering:
    /// 1. hard-coded defaults, then
    /// 2. the supported `.npmrc` subset read from the nearest `.npmrc`
    ///    (cwd, falling back to home), then
    /// 3. the nearest `pnpm-workspace.yaml` walking up from cwd.
    ///
    /// Pacquet currently only applies `registry` from `.npmrc`. Other
    /// `.npmrc` entries — pnpm's TLS / npm-auth / proxy / scoped-registry
    /// keys, plus project-structural settings like `storeDir`, `lockfile`
    /// and `hoist-pattern` — are silently ignored here. The first group
    /// is tracked for future auth / proxy / TLS work; the second must
    /// come from `pnpm-workspace.yaml` or CLI flags, matching pnpm 11.
    ///
    /// The yaml wins over `.npmrc` on any key it sets.
    ///
    /// Returns [`LoadWorkspaceYamlError`] when an existing
    /// `pnpm-workspace.yaml` cannot be read or parsed, matching pnpm's
    /// [`readWorkspaceManifest`](https://github.com/pnpm/pnpm/blob/8eb1be4988/workspace/workspace-manifest-reader/src/index.ts).
    /// A missing file is not an error.
    pub fn current<Error, CurrentDir, HomeDir, Default>(
        current_dir: CurrentDir,
        home_dir: HomeDir,
        default: Default,
    ) -> Result<Self, LoadWorkspaceYamlError>
    where
        CurrentDir: FnOnce() -> Result<PathBuf, Error>,
        HomeDir: FnOnce() -> Option<PathBuf>,
        Default: FnOnce() -> Config,
    {
        let mut config = default();

        let cwd = current_dir().ok();
        // Re-anchor the path-valued defaults (`modules_dir`,
        // `virtual_store_dir`) onto the caller-supplied cwd. SmartDefault
        // populates them via [`defaults::default_modules_dir`] /
        // [`defaults::default_virtual_store_dir`], which both anchor at
        // `env::current_dir()`. That diverges from `cwd` whenever the
        // caller passed a different directory (notably
        // `pacquet --dir <path>` from elsewhere), so without this fixup
        // pacquet would load config from `<path>` while still installing
        // to the process-cwd `node_modules`. Matches pnpm 11, whose
        // `modulesDir`/`virtualStoreDir` defaults are resolved against
        // `pnpmConfig.dir`.
        if let Some(start) = &cwd {
            config.modules_dir = start.join("node_modules");
            config.virtual_store_dir = start.join("node_modules/.pnpm");
        }

        // Read the nearest .npmrc (cwd first, home second) and apply only
        // the auth/network subset. Everything else is intentionally ignored.
        let auth_source = cwd
            .as_ref()
            .and_then(|dir| read_npmrc(dir))
            .or_else(|| home_dir().and_then(|dir| read_npmrc(&dir)));
        if let Some(text) = auth_source {
            crate::npmrc_auth::NpmrcAuth::from_ini(&text).apply_to(&mut config);
        }

        // Layer pnpm-workspace.yaml overrides on top. A missing file is
        // silent. Read or parse failures propagate to the caller.
        if let Some(start) = cwd
            && let Some((path, settings)) = WorkspaceSettings::find_and_load(&start)?
        {
            let base_dir = path.parent().unwrap_or(&start).to_path_buf();
            settings.apply_to(&mut config, &base_dir);
        }

        Ok(config)
    }

    /// Persist the config data until the program terminates.
    pub fn leak(self) -> &'static mut Self {
        self.pipe(Box::new).pipe(Box::leak)
    }
}

/// Read the text of the `.npmrc` in `dir`, returning `None` for anything
/// from "file doesn't exist" to "not valid UTF-8" — same best-effort
/// behaviour as pnpm. The caller decides which keys to honour.
fn read_npmrc(dir: &std::path::Path) -> Option<String> {
    fs::read_to_string(dir.join(".npmrc")).ok()
}

#[cfg(test)]
mod tests {
    use std::env;

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::{Config, NodeLinker, PackageImportMethod, fs};
    use crate::{defaults::default_store_dir, test_env_guard::EnvGuard};
    use pacquet_store_dir::StoreDir;
    use pipe_trait::Pipe;

    fn display_store_dir(store_dir: &StoreDir) -> String {
        store_dir.display().to_string().replace('\\', "/")
    }

    #[test]
    pub fn have_default_values() {
        let value = Config::new();
        assert_eq!(value.node_linker, NodeLinker::default());
        assert_eq!(value.package_import_method, PackageImportMethod::default());
        assert!(value.prefer_frozen_lockfile);
        assert!(value.symlink);
        assert!(value.hoist);
        assert_eq!(value.store_dir, default_store_dir());
        assert_eq!(value.registry, "https://registry.npmjs.org/");
    }

    /// `fetch-retries*` defaults must match pnpm's
    /// `config/config/src/index.ts` (`2`, `10`, `10000`, `60000`) — these
    /// are the values pnpm bakes into npm-style fetches and we want
    /// pacquet to behave identically out of the box.
    #[test]
    pub fn fetch_retries_defaults_match_pnpm() {
        let value = Config::new();
        assert_eq!(value.fetch_retries, 2);
        assert_eq!(value.fetch_retry_factor, 10);
        assert_eq!(value.fetch_retry_mintimeout, 10_000);
        assert_eq!(value.fetch_retry_maxtimeout, 60_000);
    }

    #[test]
    pub fn should_use_pnpm_home_env_var() {
        let _g = EnvGuard::snapshot(["PNPM_HOME"]);
        // SAFETY: EnvGuard above serializes the test against other env-mutating
        // tests in this process; no other thread reads these vars concurrently.
        unsafe {
            env::set_var("PNPM_HOME", "/hello"); // TODO: change this to dependency injection
        }
        let value = Config::new();
        assert_eq!(display_store_dir(&value.store_dir), "/hello/store");
    }

    #[test]
    pub fn should_use_xdg_data_home_env_var() {
        // Clear `PNPM_HOME` first — `default_store_dir` checks it
        // before `XDG_DATA_HOME`, so running the test suite with pnpm
        // installed (common) would otherwise hit the `PNPM_HOME`
        // branch and fail the assertion. Snapshot both so the test
        // cleans up after itself even when parallel peers observe the
        // temporarily-unset state. See the companion fix in
        // `defaults::tests::test_default_store_dir_with_xdg_env`.
        let _g = EnvGuard::snapshot(["PNPM_HOME", "XDG_DATA_HOME"]);
        // SAFETY: EnvGuard above serializes the test against other env-mutating
        // tests in this process; no other thread reads these vars concurrently.
        unsafe {
            env::remove_var("PNPM_HOME"); // TODO: change this to dependency injection
            env::set_var("XDG_DATA_HOME", "/hello");
        }
        let value = Config::new();
        assert_eq!(display_store_dir(&value.store_dir), "/hello/pnpm/store");
    }

    #[test]
    pub fn npmrc_in_current_folder_applies_registry() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".npmrc"), "registry=https://cwd.example")
            .expect("write to .npmrc");
        let config = Config::current(
            || tmp.path().to_path_buf().pipe(Ok::<_, ()>),
            || unreachable!("shouldn't reach home dir"),
            Config::new,
        )
        .expect("workspace yaml absent => no error");
        assert_eq!(config.registry, "https://cwd.example/");
    }

    #[test]
    pub fn non_auth_keys_in_npmrc_are_ignored() {
        // pnpm 11 stopped reading project-structural settings from .npmrc.
        // Writing `symlink=false` / `lockfile=true` / hoist / node-linker /
        // store-dir to .npmrc should have no effect on the resolved config.
        let tmp = tempdir().unwrap();
        let non_auth_ini = "symlink=false\nlockfile=true\nhoist=false\nnode-linker=hoisted\n";
        fs::write(tmp.path().join(".npmrc"), non_auth_ini).expect("write to .npmrc");
        let defaults = Config::new();
        let config =
            Config::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Config::new)
                .expect("workspace yaml absent => no error");
        assert_eq!(config.symlink, defaults.symlink);
        assert_eq!(config.lockfile, defaults.lockfile);
        assert_eq!(config.hoist, defaults.hoist);
        assert_eq!(config.node_linker, defaults.node_linker);
    }

    /// pnpm 11's `isIniConfigKey` (config/config/src/auth.ts) leaves the
    /// `fetch-retries*` family out of `NPM_AUTH_SETTINGS`, so a value
    /// like `fetch-retries=99` in `.npmrc` is silently ignored upstream.
    /// pacquet must do the same — applying it would diverge from pnpm
    /// and silently change install behaviour for projects that have a
    /// stale `.npmrc` lying around.
    #[test]
    pub fn fetch_retry_keys_in_npmrc_are_ignored() {
        let tmp = tempdir().unwrap();
        let ini = "fetch-retries=99\nfetch-retry-factor=99\nfetch-retry-mintimeout=99\nfetch-retry-maxtimeout=99\n";
        fs::write(tmp.path().join(".npmrc"), ini).expect("write to .npmrc");
        let defaults = Config::new();
        let config =
            Config::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Config::new)
                .expect("workspace yaml absent => no error");
        assert_eq!(config.fetch_retries, defaults.fetch_retries);
        assert_eq!(config.fetch_retry_factor, defaults.fetch_retry_factor);
        assert_eq!(config.fetch_retry_mintimeout, defaults.fetch_retry_mintimeout);
        assert_eq!(config.fetch_retry_maxtimeout, defaults.fetch_retry_maxtimeout);
    }

    #[test]
    pub fn test_current_folder_for_invalid_npmrc() {
        let tmp = tempdir().unwrap();
        // write invalid utf-8 value to npmrc
        fs::write(tmp.path().join(".npmrc"), b"Hello \xff World").expect("write to .npmrc");
        let config =
            Config::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Config::new)
                .expect("workspace yaml absent => no error");
        assert!(config.symlink); // default — invalid .npmrc is silently ignored
    }

    #[test]
    pub fn npmrc_in_home_folder_applies_registry() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        fs::write(home_dir.path().join(".npmrc"), "registry=https://home.example")
            .expect("write to .npmrc");
        let config = Config::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            Config::new,
        )
        .expect("workspace yaml absent => no error");
        assert_eq!(config.registry, "https://home.example/");
    }

    #[test]
    pub fn pnpm_workspace_yaml_registry_overrides_npmrc_registry() {
        // `registry` is the one non-scope key pnpm 11 still reads from
        // .npmrc (it's in RAW_AUTH_CFG_KEYS). When both files define it,
        // the yaml wins, matching pnpm itself.
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".npmrc"), "registry=https://from-npmrc.test")
            .expect("write to .npmrc");
        fs::write(tmp.path().join("pnpm-workspace.yaml"), "registry: https://from-yaml.test\n")
            .expect("write to pnpm-workspace.yaml");
        let config = Config::current(
            || tmp.path().to_path_buf().pipe(Ok::<_, ()>),
            || unreachable!("shouldn't reach home dir"),
            Config::new,
        )
        .expect("yaml is valid");
        assert_eq!(config.registry, "https://from-yaml.test/");
    }

    #[test]
    pub fn pnpm_workspace_yaml_found_by_walking_up() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("packages/inner");
        fs::create_dir_all(&nested).unwrap();
        fs::write(tmp.path().join("pnpm-workspace.yaml"), "symlink: false\n")
            .expect("write to pnpm-workspace.yaml");
        // No `.npmrc` anywhere, but a parent dir has `pnpm-workspace.yaml` —
        // the yaml should still be applied.
        let config = Config::current(|| nested.clone().pipe(Ok::<_, ()>), || None, Config::new)
            .expect("yaml is valid");
        assert!(!config.symlink);
    }

    #[test]
    pub fn test_current_folder_fallback_to_default() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        let config = Config::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            || Config { symlink: false, ..Config::new() },
        )
        .expect("workspace yaml absent => no error");
        assert!(!config.symlink);
    }

    /// Pnpm's
    /// [`workspace-manifest-reader`](https://github.com/pnpm/pnpm/blob/8eb1be4988/workspace/workspace-manifest-reader/src/index.ts)
    /// fails the process on invalid yaml. `Config::current` must do the
    /// same instead of silently falling back to defaults.
    #[test]
    pub fn invalid_workspace_yaml_propagates_error() {
        let tmp = tempdir().unwrap();
        // `: : :` is rejected by saphyr.
        fs::write(tmp.path().join("pnpm-workspace.yaml"), ": : :\n")
            .expect("write to pnpm-workspace.yaml");
        let result =
            Config::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Config::new);
        let err = result.expect_err("invalid yaml should error");
        assert!(
            matches!(err, crate::LoadWorkspaceYamlError::ParseYaml { .. }),
            "expected ParseYaml, got {err:?}",
        );
    }
}
