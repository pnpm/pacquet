mod package;

use std::{
    collections::HashMap,
    env, fs,
    io::{Read, Write},
    path::PathBuf,
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::package::LockfilePackage;

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
pub struct LockfileSettings {
    #[serde(alias = "autoInstallPeers")]
    auto_install_peers: bool,
    #[serde(alias = "excludeLinksFromLockfile")]
    exclude_links_from_lockfile: bool,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Lockfile {
    #[serde(alias = "lockFileVersion")]
    pub lock_file_version: String,
    pub settings: Option<LockfileSettings>,
    #[serde(alias = "neverBuiltDependencies")]
    pub never_built_dependencies: Option<Vec<String>>,
    pub overrides: Option<HashMap<String, String>>,
    pub dependencies: Option<HashMap<String, LockfileDependency>>,
    pub packages: Option<HashMap<String, LockfilePackage>>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Self::new()
    }
}

impl Lockfile {
    pub fn path() -> Result<PathBuf, LockfileError> {
        Ok(env::current_dir()?.join("pacquet-lock.yaml"))
    }

    pub fn new() -> Self {
        Lockfile {
            lock_file_version: "6.0".to_string(),
            settings: Some(LockfileSettings {
                auto_install_peers: true,
                exclude_links_from_lockfile: false,
            }),
            never_built_dependencies: None,
            overrides: None,
            dependencies: None,
            packages: None,
        }
    }

    pub fn create() -> Result<Self, LockfileError> {
        let file = Lockfile::new();
        file.save()?;
        Ok(file)
    }

    pub fn open() -> Result<Lockfile, LockfileError> {
        let yaml_path = Lockfile::path()?;
        let mut file = fs::File::open(yaml_path)?;
        let mut buffer = String::new();
        file.read_to_string(&mut buffer)?;
        let lockfile: Lockfile = serde_yaml::from_str(&buffer)?;
        Ok(lockfile)
    }

    pub fn create_or_open() -> Result<Lockfile, LockfileError> {
        let yaml_path = Lockfile::path()?;
        if yaml_path.exists() {
            Ok(Lockfile::open()?)
        } else {
            Ok(Lockfile::create()?)
        }
    }

    pub fn save(&self) -> Result<(), LockfileError> {
        let yaml_path = Lockfile::path()?;
        let mut file = fs::File::create(yaml_path)?;
        let yaml = serde_yaml::to_string(&self)?;
        file.write_all(yaml.as_bytes())?;
        Ok(())
    }
}
