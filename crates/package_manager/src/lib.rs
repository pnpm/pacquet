use std::path::PathBuf;

use pacquet_npmrc::{get_current_npmrc, Npmrc};
use pacquet_package_json::PackageJson;
use pacquet_registry::RegistryManager;
use pacquet_tarball::TarballManager;
use thiserror::Error;

mod commands;
mod package_import;
mod symlink;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum PackageManagerError {
    #[error("tarball error")]
    Tarball(#[from] pacquet_tarball::TarballError),
    #[error("package.json error")]
    PackageJson(#[from] pacquet_package_json::error::PackageJsonError),
    #[error("registry error")]
    Registry(#[from] pacquet_registry::RegistryError),
    #[error("filesystem error")]
    FileSystem(#[from] std::io::Error),
    #[error("content addressable store error")]
    Cache(#[from] cacache::Error),
}

pub struct PackageManager {
    config: Box<Npmrc>,
    package_json: Box<PackageJson>,
    registry: Box<RegistryManager>,
    tarball: Box<TarballManager>,
}

impl PackageManager {
    pub fn new<P: Into<PathBuf>>(package_json_path: P) -> Result<Self, PackageManagerError> {
        let config = get_current_npmrc();
        Ok(PackageManager {
            registry: Box::new(RegistryManager::new(&config.registry)),
            tarball: Box::new(TarballManager::new(&config.store_dir)),
            config: Box::new(config),
            package_json: Box::new(PackageJson::create_if_needed(&package_json_path.into())?),
        })
    }
}
