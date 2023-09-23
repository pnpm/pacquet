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
    DirectoryResolution, GitResolution, IntegrityResolution, LockfileResolution, TarballResolution,
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
    pub settings: Option<LockfileSettings>,
    pub never_built_dependencies: Option<Vec<String>>,
    pub overrides: Option<HashMap<String, String>>,
    pub dependencies: Option<HashMap<String, LockfileDependency>>,
    pub packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pacquet-lock.yaml";
}
