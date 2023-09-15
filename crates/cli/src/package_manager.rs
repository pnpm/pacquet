use std::{io, path::PathBuf};

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use pacquet_npmrc::Npmrc;
use pacquet_package_json::PackageJson;
use pacquet_tarball::Cache;

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
    pub config: &'static Npmrc,
    pub package_json: PackageJson,
    pub http_client: reqwest::Client,
    pub(crate) tarball_cache: Cache,
}

impl PackageManager {
    pub fn new<P: Into<PathBuf>>(
        package_json_path: P,
        config: &'static Npmrc,
    ) -> Result<Self, PackageManagerError> {
        Ok(PackageManager {
            config,
            package_json: PackageJson::create_if_needed(package_json_path.into())?,
            http_client: reqwest::Client::new(),
            tarball_cache: Cache::new(),
        })
    }
}
