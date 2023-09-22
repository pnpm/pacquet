mod comver;
mod dependency_path;
mod package;

pub use comver::{ComVer, ParseComVerError};
pub use dependency_path::DependencyPath;
pub use package::{LockfilePackage, LockfilePackageResolution};

use std::collections::HashMap;

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use serde::{Deserialize, Serialize};

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum LockfileError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_lockfile::io_error))]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_lockfile::serialization_error))]
    Serialization(#[from] serde_yaml::Error),
}

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
