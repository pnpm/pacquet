mod package;
mod package_distribution;
mod package_tag;
mod package_version;

pub use package::Package;
pub use package_distribution::PackageDistribution;
pub use package_tag::PackageTag;
pub use package_version::PackageVersion;

use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("failed to request {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    #[source]
    pub error: reqwest::Error,
}

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
    Network(#[from] NetworkError),

    #[error(transparent)]
    #[diagnostic(code(pacquet_registry::io_error))]
    Io(#[from] std::io::Error),

    #[error("serialization failed: {0}")]
    #[diagnostic(code(pacquet_registry::serialization_error))]
    Serialization(String),
}
