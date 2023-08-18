use std::{io, path::PathBuf};

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use pacquet_npmrc::{current_npmrc, Npmrc};
use pacquet_package_json::PackageJson;

use crate::package_cache::PackageCache;

#[derive(Error, Debug, Diagnostic)]
pub enum AutoImportError {
    #[error("cannot create directory at {dirname:?}: {error}")]
    CreateDir {
        dirname: PathBuf,
        #[source]
        error: io::Error,
    },
    #[error("fail to create a link from {from:?} to {to:?}: {error}")]
    CreateLink {
        from: PathBuf,
        to: PathBuf,
        #[source]
        error: io::Error,
    },
}

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum PackageManagerError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Tarball(#[from] pacquet_tarball::TarballError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    PackageJson(#[from] pacquet_package_json::PackageJsonError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Registry(#[from] pacquet_registry::RegistryError),

    #[error(transparent)]
    #[diagnostic(code(pacquet_package_manager::io_error))]
    Io(#[from] io::Error),

    #[error(transparent)]
    #[diagnostic(transparent)]
    AutoImport(#[from] AutoImportError),
}

pub struct PackageManager {
    pub config: Box<Npmrc>,
    pub package_json: Box<PackageJson>,
    pub http_client: Box<reqwest::Client>,
    pub(crate) package_cache: PackageCache,
}

impl PackageManager {
    pub fn new<P: Into<PathBuf>>(package_json_path: P) -> Result<Self, PackageManagerError> {
        Ok(PackageManager {
            config: Box::new(current_npmrc()),
            package_json: Box::new(PackageJson::create_if_needed(package_json_path.into())?),
            http_client: Box::new(reqwest::Client::new()),
            package_cache: PackageCache::new(),
        })
    }
}
