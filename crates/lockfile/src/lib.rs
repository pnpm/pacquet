mod comver;
mod dependency;
mod dependency_path;
mod load_lockfile;
mod package_snapshot;
mod resolution;

pub use comver::{ComVer, ParseComVerError};
pub use dependency::LockfileDependency;
pub use dependency_path::DependencyPath;
pub use load_lockfile::LoadLockfileError;
pub use package_snapshot::{LockfilePeerDependencyMetaValue, PackageSnapshot};
pub use resolution::{
    DirectoryResolution, GitResolution, LockfileResolution, RegistryResolution, TarballResolution,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockfileSettings {
    auto_install_peers: bool,
    exclude_links_from_lockfile: bool,
}

/// * Specification: https://github.com/pnpm/spec/blob/master/lockfile/6.0.md
/// * Reference: https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-types/src/index.ts
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: ComVer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<LockfileSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub never_built_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<HashMap<String, LockfileDependency>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pnpm-lock.yaml";
}
