mod comver;
mod dependency_path;
mod package;

pub use comver::{ComVer, ParseComVerError};
pub use dependency_path::DependencyPath;
pub use package::{LockfilePackage, LockfilePackageResolution};
use pipe_trait::Pipe;

use std::{
    collections::HashMap,
    env, fs,
    io::{self, ErrorKind},
};

use derive_more::{Display, Error};
use pacquet_diagnostics::miette::{self, Diagnostic};
use serde::{Deserialize, Serialize};

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

/// Error when reading lockfile the filesystem.
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum LoadLockfileError {
    #[display(fmt = "Failed to get current_dir: {_0}")]
    #[diagnostic(code(pacquet_lockfile::current_dir))]
    CurrentDir(io::Error),

    #[display(fmt = "Failed to read lockfile content: {_0}")]
    #[diagnostic(code(pacquet_lockfile::read_file))]
    ReadFile(io::Error),

    #[display(fmt = "Failed to parse lockfile content as YAML: {_0}")]
    #[diagnostic(code(pacquet_lockfile::parse_yaml))]
    ParseYaml(serde_yaml::Error),
}

impl Lockfile {
    /// Base file name of the lockfile.
    const FILE_NAME: &str = "pacquet-lock.yaml";

    /// Load lockfile from the current directory.
    pub fn load_from_current_dir() -> Result<Option<Self>, LoadLockfileError> {
        let file_path =
            env::current_dir().map_err(LoadLockfileError::CurrentDir)?.join(Lockfile::FILE_NAME);
        let content = match fs::read_to_string(file_path) {
            Ok(content) => content,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return error.pipe(LoadLockfileError::ReadFile).pipe(Err),
        };
        content.pipe_as_ref(serde_yaml::from_str).map_err(LoadLockfileError::ParseYaml)
    }
}
