use thiserror::Error;

#[derive(Error, Debug)]
pub enum PackageJsonError {
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("io error: `{0}`")]
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
