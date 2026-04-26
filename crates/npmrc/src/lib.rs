mod custom_deserializer;
mod npmrc_auth;
#[cfg(test)]
mod test_env_guard;
mod workspace_yaml;

use pacquet_store_dir::StoreDir;
use pipe_trait::Pipe;
use serde::Deserialize;
use std::{fs, path::PathBuf};

use crate::custom_deserializer::{
    bool_true, default_fetch_retries, default_fetch_retry_factor, default_fetch_retry_maxtimeout,
    default_fetch_retry_mintimeout, default_hoist_pattern, default_modules_cache_max_age,
    default_modules_dir, default_public_hoist_pattern, default_registry, default_store_dir,
    default_virtual_store_dir, deserialize_bool, deserialize_pathbuf, deserialize_registry,
    deserialize_store_dir, deserialize_u32, deserialize_u64,
};
pub use workspace_yaml::{
    workspace_root_or, LoadWorkspaceYamlError, WorkspaceSettings, WORKSPACE_MANIFEST_FILENAME,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Npmrc {
    /// When true, all dependencies are hoisted to node_modules/.pnpm/node_modules.
    /// This makes unlisted dependencies accessible to all packages inside node_modules.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub hoist: bool,

    /// Tells pnpm which packages should be hoisted to node_modules/.pnpm/node_modules.
    /// By default, all packages are hoisted - however, if you know that only some flawed packages
    /// have phantom dependencies, you can use this option to exclusively hoist the phantom
    /// dependencies (recommended).
    #[serde(default = "default_hoist_pattern")]
    pub hoist_pattern: Vec<String>,

    /// Unlike hoist-pattern, which hoists dependencies to a hidden modules directory inside the
    /// virtual store, public-hoist-pattern hoists dependencies matching the pattern to the root
    /// modules directory. Hoisting to the root modules directory means that application code will
    /// have access to phantom dependencies, even if they modify the resolution strategy improperly.
    #[serde(default = "default_public_hoist_pattern")]
    pub public_hoist_pattern: Vec<String>,

    /// By default, pnpm creates a semistrict node_modules, meaning dependencies have access to
    /// undeclared dependencies but modules outside of node_modules do not. With this layout,
    /// most of the packages in the ecosystem work with no issues. However, if some tooling only
    /// works when the hoisted dependencies are in the root of node_modules, you can set this to
    /// true to hoist them for you.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub shamefully_hoist: bool,

    /// The location where all the packages are saved on the disk.
    #[serde(default = "default_store_dir", deserialize_with = "deserialize_store_dir")]
    pub store_dir: StoreDir,

    /// The directory in which dependencies will be installed (instead of node_modules).
    #[serde(default = "default_modules_dir", deserialize_with = "deserialize_pathbuf")]
    pub modules_dir: PathBuf,

    /// Defines what linker should be used for installing Node packages.
    #[serde(default)]
    pub node_linker: NodeLinker,

    /// When symlink is set to false, pnpm creates a virtual store directory without any symlinks.
    /// It is a useful setting together with node-linker=pnp.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub symlink: bool,

    /// The directory with links to the store. All direct and indirect dependencies of the
    /// project are linked into this directory.
    #[serde(default = "default_virtual_store_dir", deserialize_with = "deserialize_pathbuf")]
    pub virtual_store_dir: PathBuf,

    /// Controls the way packages are imported from the store (if you want to disable symlinks
    /// inside node_modules, then you need to change the node-linker setting, not this one).
    #[serde(default)]
    pub package_import_method: PackageImportMethod,

    /// The time in minutes after which orphan packages from the modules directory should be
    /// removed. pnpm keeps a cache of packages in the modules directory. This boosts installation
    /// speed when switching branches or downgrading dependencies.
    ///
    /// Default value is 10080 (7 days in minutes)
    #[serde(default = "default_modules_cache_max_age", deserialize_with = "deserialize_u64")]
    pub modules_cache_max_age: u64,

    /// When set to false, pnpm won't read or generate a pnpm-lock.yaml file.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub lockfile: bool,

    /// When set to true and the available pnpm-lock.yaml satisfies the package.json dependencies
    /// directive, a headless installation is performed. A headless installation skips all
    /// dependency resolution as it does not need to modify the lockfile.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub prefer_frozen_lockfile: bool,

    /// Add the full URL to the package's tarball to every entry in pnpm-lock.yaml.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub lockfile_include_tarball_url: bool,

    /// The base URL of the npm package registry (trailing slash included).
    #[serde(default = "default_registry", deserialize_with = "deserialize_registry")]
    pub registry: String, // TODO: use Url type (compatible with reqwest)

    /// When true, any missing non-optional peer dependencies are automatically installed.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub auto_install_peers: bool,

    /// When this setting is set to true, packages with peer dependencies will be deduplicated after peers resolution.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub dedupe_peer_dependents: bool,

    /// If this is enabled, commands will fail if there is a missing or invalid peer dependency in the tree.
    #[serde(default, deserialize_with = "deserialize_bool")]
    pub strict_peer_dependencies: bool,

    /// When enabled, dependencies of the root workspace project are used to resolve peer
    /// dependencies of any projects in the workspace. It is a useful feature as you can install
    /// your peer dependencies only in the root of the workspace, and you can be sure that all
    /// projects in the workspace use the same versions of the peer dependencies.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
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
    /// Only `pnpm-workspace.yaml` is wired up today — [`Npmrc::current`]
    /// applies auth/registry from `.npmrc` and reads project-structural
    /// settings from `pnpm-workspace.yaml`, matching pnpm 11's own
    /// split. A `verify-store-integrity=…` line in `.npmrc` is
    /// silently ignored.
    #[serde(default = "bool_true", deserialize_with = "deserialize_bool")]
    pub verify_store_integrity: bool,

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
    /// ignored here too. The kebab-case serde attribute exists only to
    /// power [`Npmrc::new`]'s defaults; [`Npmrc::current`] applies the
    /// auth subset from `.npmrc` and reads project-structural settings
    /// from `pnpm-workspace.yaml`.
    #[serde(default = "default_fetch_retries", deserialize_with = "deserialize_u32")]
    pub fetch_retries: u32,

    /// Exponential-backoff growth factor between retry attempts. Mirrors
    /// pnpm's `fetchRetryFactor` (default `10`). Successive backoff is
    /// `min(fetch_retry_mintimeout * factor^attempt, fetch_retry_maxtimeout)`.
    /// Yaml-only — see [`Npmrc::fetch_retries`].
    #[serde(default = "default_fetch_retry_factor", deserialize_with = "deserialize_u32")]
    pub fetch_retry_factor: u32,

    /// Floor in milliseconds for the wait between retries. Mirrors pnpm's
    /// `fetchRetryMintimeout` (default `10000` — 10 s). Yaml-only — see
    /// [`Npmrc::fetch_retries`].
    #[serde(default = "default_fetch_retry_mintimeout", deserialize_with = "deserialize_u64")]
    pub fetch_retry_mintimeout: u64,

    /// Cap in milliseconds on the wait between retries. Mirrors pnpm's
    /// `fetchRetryMaxtimeout` (default `60000` — 1 min). Yaml-only —
    /// see [`Npmrc::fetch_retries`].
    #[serde(default = "default_fetch_retry_maxtimeout", deserialize_with = "deserialize_u64")]
    pub fetch_retry_maxtimeout: u64,
}

impl Npmrc {
    pub fn new() -> Self {
        let config: Npmrc = serde_ini::from_str("").unwrap(); // TODO: derive `SmartDefault` for `Npmrc and call `Npmrc::default()`
        config
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
    pub fn current<Error, CurrentDir, HomeDir, Default>(
        current_dir: CurrentDir,
        home_dir: HomeDir,
        default: Default,
    ) -> Self
    where
        CurrentDir: FnOnce() -> Result<PathBuf, Error>,
        HomeDir: FnOnce() -> Option<PathBuf>,
        Default: FnOnce() -> Npmrc,
    {
        let mut npmrc = default();

        let cwd = current_dir().ok();
        // Read the nearest .npmrc (cwd first, home second) and apply only
        // the auth/network subset. Everything else is intentionally ignored.
        let auth_source = cwd
            .as_ref()
            .and_then(|dir| read_npmrc(dir))
            .or_else(|| home_dir().and_then(|dir| read_npmrc(&dir)));
        if let Some(text) = auth_source {
            crate::npmrc_auth::NpmrcAuth::from_ini(&text).apply_to(&mut npmrc);
        }

        // Layer pnpm-workspace.yaml overrides on top. Missing file or
        // unreadable yaml is silently ignored, matching .npmrc's
        // best-effort behaviour above.
        if let Some(start) = cwd {
            if let Ok(Some((path, settings))) = WorkspaceSettings::find_and_load(&start) {
                let base_dir = path.parent().unwrap_or(&start).to_path_buf();
                settings.apply_to(&mut npmrc, &base_dir);
            }
        }

        npmrc
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

impl Default for Npmrc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::{env, str::FromStr};

    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;
    use crate::test_env_guard::EnvGuard;

    fn display_store_dir(store_dir: &StoreDir) -> String {
        store_dir.display().to_string().replace('\\', "/")
    }

    #[test]
    pub fn have_default_values() {
        let value = Npmrc::new();
        assert_eq!(value.node_linker, NodeLinker::default());
        assert_eq!(value.package_import_method, PackageImportMethod::default());
        assert!(value.prefer_frozen_lockfile);
        assert!(value.symlink);
        assert!(value.hoist);
        assert_eq!(value.store_dir, default_store_dir());
        assert_eq!(value.registry, "https://registry.npmjs.org/");
    }

    #[test]
    pub fn parse_package_import_method() {
        let value: Npmrc = serde_ini::from_str("package-import-method=hardlink").unwrap();
        assert_eq!(value.package_import_method, PackageImportMethod::Hardlink);
    }

    #[test]
    pub fn parse_node_linker() {
        let value: Npmrc = serde_ini::from_str("node-linker=hoisted").unwrap();
        assert_eq!(value.node_linker, NodeLinker::Hoisted);
    }

    #[test]
    pub fn parse_bool() {
        let value: Npmrc = serde_ini::from_str("prefer-frozen-lockfile=false").unwrap();
        assert!(!value.prefer_frozen_lockfile);
    }

    #[test]
    pub fn parse_u64() {
        let value: Npmrc = serde_ini::from_str("modules-cache-max-age=1000").unwrap();
        assert_eq!(value.modules_cache_max_age, 1000);
    }

    /// `fetch-retries*` defaults must match pnpm's
    /// `config/config/src/index.ts` (`2`, `10`, `10000`, `60000`) — these
    /// are the values pnpm bakes into npm-style fetches and we want
    /// pacquet to behave identically out of the box.
    #[test]
    pub fn fetch_retries_defaults_match_pnpm() {
        let value = Npmrc::new();
        assert_eq!(value.fetch_retries, 2);
        assert_eq!(value.fetch_retry_factor, 10);
        assert_eq!(value.fetch_retry_mintimeout, 10_000);
        assert_eq!(value.fetch_retry_maxtimeout, 60_000);
    }

    #[test]
    pub fn should_use_pnpm_home_env_var() {
        let _g = EnvGuard::snapshot(["PNPM_HOME"]);
        env::set_var("PNPM_HOME", "/hello"); // TODO: change this to dependency injection
        let value: Npmrc = serde_ini::from_str("").unwrap();
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
        // `custom_deserializer::tests::test_default_store_dir_with_xdg_env`.
        let _g = EnvGuard::snapshot(["PNPM_HOME", "XDG_DATA_HOME"]);
        env::remove_var("PNPM_HOME"); // TODO: change this to dependency injection
        env::set_var("XDG_DATA_HOME", "/hello");
        let value: Npmrc = serde_ini::from_str("").unwrap();
        assert_eq!(display_store_dir(&value.store_dir), "/hello/pnpm/store");
    }

    #[test]
    pub fn should_use_relative_virtual_store_dir() {
        let value: Npmrc = serde_ini::from_str("virtual-store-dir=node_modules/.pacquet").unwrap();
        assert_eq!(
            value.virtual_store_dir,
            env::current_dir().unwrap().join("node_modules/.pacquet")
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    pub fn should_use_absolute_virtual_store_dir() {
        let value: Npmrc = serde_ini::from_str("virtual-store-dir=/node_modules/.pacquet").unwrap();
        assert_eq!(value.virtual_store_dir, PathBuf::from_str("/node_modules/.pacquet").unwrap());
    }

    #[test]
    pub fn add_slash_to_registry_end() {
        let without_slash: Npmrc = serde_ini::from_str("registry=https://yagiz.co").unwrap();
        assert_eq!(without_slash.registry, "https://yagiz.co/");

        let without_slash: Npmrc = serde_ini::from_str("registry=https://yagiz.co/").unwrap();
        assert_eq!(without_slash.registry, "https://yagiz.co/");
    }

    #[test]
    pub fn npmrc_in_current_folder_applies_registry() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join(".npmrc"), "registry=https://cwd.example")
            .expect("write to .npmrc");
        let config = Npmrc::current(
            || tmp.path().to_path_buf().pipe(Ok::<_, ()>),
            || unreachable!("shouldn't reach home dir"),
            Npmrc::new,
        );
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
        let defaults = Npmrc::new();
        let config =
            Npmrc::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Npmrc::new);
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
        let defaults = Npmrc::new();
        let config =
            Npmrc::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Npmrc::new);
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
            Npmrc::current(|| tmp.path().to_path_buf().pipe(Ok::<_, ()>), || None, Npmrc::new);
        assert!(config.symlink); // default — invalid .npmrc is silently ignored
    }

    #[test]
    pub fn npmrc_in_home_folder_applies_registry() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        fs::write(home_dir.path().join(".npmrc"), "registry=https://home.example")
            .expect("write to .npmrc");
        let config = Npmrc::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            Npmrc::new,
        );
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
        let config = Npmrc::current(
            || tmp.path().to_path_buf().pipe(Ok::<_, ()>),
            || unreachable!("shouldn't reach home dir"),
            Npmrc::new,
        );
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
        let config = Npmrc::current(|| nested.clone().pipe(Ok::<_, ()>), || None, Npmrc::new);
        assert!(!config.symlink);
    }

    #[test]
    pub fn test_current_folder_fallback_to_default() {
        let current_dir = tempdir().unwrap();
        let home_dir = tempdir().unwrap();
        let config = Npmrc::current(
            || current_dir.path().to_path_buf().pipe(Ok::<_, ()>),
            || home_dir.path().to_path_buf().pipe(Some),
            || serde_ini::from_str("symlink=false").unwrap(),
        );
        assert!(!config.symlink);
    }
}
