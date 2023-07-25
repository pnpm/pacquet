pub mod package;

use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use thiserror::Error;

use crate::package::{Package, PackageVersion};

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("missing latest tag on {0}")]
    MissingLatestTag(String),
    #[error("missing version {0} on package {0}")]
    MissingVersionRelease(String, String),
    #[error("network error while fetching {0}")]
    Network(#[from] reqwest::Error),
    #[error("network middleware error")]
    NetworkMiddleware(#[from] reqwest_middleware::Error),
    #[error("io error {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("tarball error: {0}")]
    Tarball(#[from] pacquet_tarball::TarballError),
    #[error("package.json error: {0}")]
    PackageJson(#[from] pacquet_package_json::error::PackageJsonError),
}

pub struct RegistryManager {
    client: ClientWithMiddleware,
    cache: elsa::FrozenMap<String, Box<Package>>,
    registry: String,
}

impl RegistryManager {
    pub fn new(registry: &str) -> Self {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = ClientBuilder::new(reqwest::Client::new())
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();

        Self { client, cache: elsa::FrozenMap::new(), registry: registry.to_string() }
    }

    pub async fn get_package(&self, name: &str) -> Result<&Package, RegistryError> {
        if let Some(package) = &self.cache.get(name) {
            return Ok(package);
        }

        let package: Package = self
            .client
            .get(format!("{0}{name}", &self.registry))
            .header("user-agent", "pacquet-cli")
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<Package>()
            .await?;

        let package = self.cache.insert(name.to_string(), Box::new(package));

        Ok(package)
    }

    pub async fn get_package_by_version(
        &self,
        name: &str,
        version: &str,
    ) -> Result<PackageVersion, RegistryError> {
        Ok(self
            .client
            .get(format!("{0}{name}/{version}", &self.registry))
            .header("user-agent", "pacquet-cli")
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<PackageVersion>()
            .await?)
    }
}
