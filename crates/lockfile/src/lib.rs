mod comver;
mod dependency_path;
mod load_lockfile;
mod package;

pub use comver::{ComVer, ParseComVerError};
pub use dependency_path::DependencyPath;
pub use load_lockfile::LoadLockfileError;
pub use package::{LockfilePackage, LockfilePackageResolution};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LockfileDependency {
    specifier: String,
    version: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct LockfilePeerDependencyMeta {
    optional: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockfileSettings {
    auto_install_peers: bool,
    exclude_links_from_lockfile: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: ComVer,
    pub settings: Option<LockfileSettings>,
    pub never_built_dependencies: Option<Vec<String>>,
    pub overrides: Option<HashMap<String, String>>,
    pub dependencies: Option<HashMap<String, LockfileDependency>>,
    pub packages: Option<HashMap<DependencyPath, LockfilePackage>>,
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pacquet-lock.yaml";
}
