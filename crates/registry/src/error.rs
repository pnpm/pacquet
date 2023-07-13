use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("missing latest tag on `{0}`")]
    MissingLatestTag(String),
    #[error("missing version `{0}` on package `${0}`")]
    MissingVersionRelease(String, String),
    #[error("network error while downloading `${0}`")]
    Network(#[from] reqwest::Error),
    #[error("io error `${0}`")]
    Io(#[from] std::io::Error),
    #[error("serialization failed: `${0}")]
    Serialization(String),
}
