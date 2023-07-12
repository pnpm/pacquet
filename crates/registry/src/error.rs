use thiserror::Error;

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("missing latest tag on `{0}`")]
    MissingLatestTag(String),
    #[error("missing version `{0}` on package `${0}`")]
    MissingVersionRelease(String, String),
    #[error("network error while downloading `${0}`")]
    Network(String),
    #[error("filesystem error: `{0}`")]
    FileSystem(String),
    #[error("serialization failed: `${0}")]
    Serialization(String),
}
