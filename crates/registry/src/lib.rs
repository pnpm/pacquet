pub mod package;
pub mod package_distribution;
pub mod package_version;

use crate::{package::Package, package_version::PackageVersion};
use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
};

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
}

pub async fn get_package_from_registry(
    name: &str,
    http_client: &reqwest::Client,
    registry: &str,
) -> Result<Package, RegistryError> {
    let package: Package = http_client
        .get(format!("{0}{name}", &registry))
        .header("user-agent", "pacquet-cli")
        .header("content-type", "application/json")
        .send()
        .await?
        .json::<Package>()
        .await?;

    Ok(package)
}

pub async fn get_package_version_from_registry(
    name: &str,
    version: &str,
    http_client: &reqwest::Client,
    registry: &str,
) -> Result<PackageVersion, RegistryError> {
    let package_version: PackageVersion = http_client
        .get(format!("{0}{name}/{version}", &registry))
        .header("user-agent", "pacquet-cli")
        .header("content-type", "application/json")
        .send()
        .await?
        .json::<PackageVersion>()
        .await?;

    Ok(package_version)
}
