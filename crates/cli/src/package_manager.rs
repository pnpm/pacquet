use http_cache_reqwest::{Cache, CacheMode, HttpCache, HttpCacheOptions, MokaManager};
use reqwest::Client;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use std::path::PathBuf;

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};
use pacquet_npmrc::{get_current_npmrc, Npmrc};
use pacquet_package_json::PackageJson;

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
    Io(#[from] std::io::Error),
}

pub struct PackageManager {
    pub config: Box<Npmrc>,
    pub package_json: Box<PackageJson>,
    pub http_client: Box<ClientWithMiddleware>,
}

impl PackageManager {
    pub fn new<P: Into<PathBuf>>(package_json_path: P) -> Result<Self, PackageManagerError> {
        Ok(PackageManager {
            config: Box::new(get_current_npmrc()),
            package_json: Box::new(PackageJson::create_if_needed(package_json_path.into())?),
            http_client: Box::new(
                ClientBuilder::new(Client::new())
                    .with(Cache(HttpCache {
                        mode: CacheMode::ForceCache,
                        manager: MokaManager::default(),
                        options: HttpCacheOptions::default(),
                    }))
                    .build(),
            ),
        })
    }
}
