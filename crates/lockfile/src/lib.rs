mod comver;
mod dependency;
mod dependency_path;
mod load_lockfile;
mod lockfile_version;
mod package_snapshot;
mod pkg_name_suffix;
mod project_snapshot;
mod resolution;

pub use comver::{ComVer, ParseComVerError};
pub use dependency::LockfileDependency;
pub use dependency_path::DependencyPath;
pub use load_lockfile::LoadLockfileError;
pub use lockfile_version::LockfileVersion;
pub use package_snapshot::{LockfilePeerDependencyMetaValue, PackageSnapshot};
pub use pkg_name_suffix::{ParsePkgNameSuffixError, PkgNameSuffix, PkgNameVer};
pub use project_snapshot::{MultiProjectSnapshot, ProjectSnapshot, RootProjectSnapshot};
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

/// * Specification: <https://github.com/pnpm/spec/blob/master/lockfile/6.0.md>
/// * Reference: <https://github.com/pnpm/pnpm/blob/main/lockfile/lockfile-types/src/index.ts>
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: LockfileVersion<6>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<LockfileSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub never_built_dependencies: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<HashMap<String, String>>,
    #[serde(flatten)]
    pub project_snapshot: RootProjectSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<HashMap<DependencyPath, PackageSnapshot>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pnpm-lock.yaml";
}
