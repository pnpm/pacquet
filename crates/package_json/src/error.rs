use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum PackageJsonError {
    #[error("serialization failed with {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("package.json file already exists")]
    AlreadyExist,
    #[error("invalid attribute: {0}")]
    InvalidAttribute(String),
    #[error("No package.json was found in {0}")]
    NoImporterManifestFound(String),
    #[error("Missing script: \"{0}\"")]
    NoScript(String),
}
