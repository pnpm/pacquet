use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("missing latest tag on `{0}`")]
    MissingLatestTag(String),
    #[error("missing version `{0}` on package `${0}`")]
    MissingVersionRelease(String, String),
    #[error("network error while downloading `${0}`")]
    Network(reqwest::Error),
    #[error("io error `${0}`")]
    Io(std::io::Error),
    #[error("serialization failed: `${0}")]
    Serialization(String),
}

impl From<std::io::Error> for RegistryError {
    fn from(value: std::io::Error) -> Self {
        RegistryError::Io(value)
    }
}

impl From<reqwest::Error> for RegistryError {
    fn from(value: reqwest::Error) -> Self {
        RegistryError::Network(value)
    }
}
