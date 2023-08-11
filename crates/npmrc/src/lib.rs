mod custom_deserializer;

use serde::Deserialize;
use std::{env, fs, path::PathBuf};

use crate::custom_deserializer::{
    bool_true, default_hoist_pattern, default_modules_cache_max_age, default_modules_dir,
    default_public_hoist_pattern, default_registry, default_store_dir, default_virtual_store_dir,
    deserialize_bool, deserialize_pathbuf, deserialize_registry, deserialize_u64,
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

#[derive(Debug, Deserialize, Default, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum PackageImportMethod {
    ///  try to clone packages from the store. If cloning is not supported then hardlink packages
    /// from the store. If neither cloning nor linking is possible, fall back to copying
    #[default]
    Auto,

    /// hard link packages from the store
    Hardlink,

    /// try to clone packages from the store. If cloning is not supported then fall back to copying
    Copy,

    /// copy packages from the store
    Clone,

    /// clone (AKA copy-on-write or reference link) packages from the store
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
    #[serde(default = "default_store_dir", deserialize_with = "deserialize_pathbuf")]
    pub store_dir: PathBuf,

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
    pub registry: String,

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
}

impl Npmrc {
    pub fn new() -> Self {
        let config: Npmrc = serde_ini::from_str("").unwrap(); // TODO: derive `SmartDefault` for `Npmrc and call `Npmrc::default()`
        config
    }
}

impl Default for Npmrc {
    fn default() -> Self {
        Self::new()
    }
}

pub fn get_current_npmrc() -> Npmrc {
    // Look for current folder `.npmrc` and if not found, look for home directory.
    let path = match env::current_dir() {
        Ok(dir) => Some(dir.join(".npmrc")),
        _ => home::home_dir().map(|dir| dir.join(".npmrc")),
    };

    if let Some(file) = path {
        if let Ok(content) = fs::read_to_string(file) {
            if let Ok(npmrc) = serde_ini::from_str(&content) {
                return npmrc;
            }
        }
    }

    Npmrc::new()
}

#[cfg(test)]
mod tests {
    use std::{env, io::Write, str::FromStr};

    use tempfile::tempdir;

    use super::*;

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

    #[test]
    pub fn should_use_pacquet_home_env_var() {
        env::set_var("PACQUET_HOME", "/hello");
        let value: Npmrc = serde_ini::from_str("").unwrap();
        assert_eq!(value.store_dir, PathBuf::from_str("/hello/store").unwrap());
        env::remove_var("PACQUET_HOME");
    }

    #[test]
    pub fn should_use_xdg_data_home_env_var() {
        env::set_var("XDG_DATA_HOME", "/hello");
        let value: Npmrc = serde_ini::from_str("").unwrap();
        assert_eq!(value.store_dir, PathBuf::from_str("/hello/pacquet/store").unwrap());
        env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    pub fn should_return_npmrc() {
        let value = get_current_npmrc();
        assert!(value.symlink);
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
    pub fn test_current_folder_for_npmrc() {
        let tmp = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        let mut f = fs::File::create(tmp.path().join(".npmrc")).expect("Unable to create file");
        f.write_all(b"symlink=false").unwrap();
        env::set_current_dir(tmp.path()).unwrap();
        let config = get_current_npmrc();
        assert!(!config.symlink);
        env::set_current_dir(current_directory).unwrap();
    }

    #[test]
    pub fn test_current_folder_for_invalid_npmrc() {
        let tmp = tempdir().unwrap();
        let current_directory = env::current_dir().unwrap();
        let mut f = fs::File::create(tmp.path().join(".npmrc")).expect("Unable to create file");
        // write invalid utf-8 value to npmrc
        f.write_all(b"Hello \xff World").unwrap();
        env::set_current_dir(tmp.path()).unwrap();
        let config = get_current_npmrc();
        assert!(config.symlink);
        env::set_current_dir(current_directory).unwrap();
    }
}
