mod comver;
mod dependency_path;
mod load_lockfile;
mod lockfile_version;
mod multi_project_snapshot;
mod package_snapshot;
mod package_snapshot_dependency;
mod pkg_name;
mod pkg_name_suffix;
mod pkg_name_ver;
mod pkg_name_ver_peer;
mod pkg_ver_peer;
mod project_snapshot;
mod resolution;
mod resolved_dependency;
mod root_project_snapshot;

pub use comver::*;
pub use dependency_path::*;
pub use load_lockfile::*;
pub use lockfile_version::*;
pub use multi_project_snapshot::*;
pub use package_snapshot::*;
pub use package_snapshot_dependency::*;
pub use pkg_name::*;
pub use pkg_name_suffix::*;
pub use pkg_name_ver::*;
pub use pkg_name_ver_peer::*;
pub use pkg_ver_peer::*;
pub use project_snapshot::*;
pub use resolution::*;
pub use resolved_dependency::*;
pub use root_project_snapshot::*;

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
