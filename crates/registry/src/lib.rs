pub mod package;
pub mod package_distribution;
pub mod package_version;

use miette::Diagnostic;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use thiserror::Error;

use crate::{package::Package, package_version::PackageVersion};

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("missing latest tag on {0}")]
    #[diagnostic(code(pacquet_registry::missing_latest_tag))]
    MissingLatestTag(String),

    #[error("missing version {0} on package {1}")]
    #[diagnostic(code(pacquet_registry::missing_version_release))]
    MissingVersionRelease(String, String),

    #[error(transparent)]
    #[diagnostic(code(pacquet_registry::network_error))]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_registry::network_middleware_error))]
    NetworkMiddleware(#[from] reqwest_middleware::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_registry::io_error))]
    Io(#[from] std::io::Error),

    #[error("serialization failed: {0}")]
    #[diagnostic(code(pacquet_registry::serialization_error))]
    Serialization(String),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Tarball(#[from] pacquet_tarball::TarballError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    PackageJson(#[from] pacquet_package_json::PackageJsonError),
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
