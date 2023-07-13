use thiserror::Error;

#[derive(Error, Debug)]
pub enum LockfileError {
    #[error("filesystem error: `{0}`")]
    FileSystem(#[from] std::io::Error),
    #[error("serialization error: `{0}")]
    Serialization(#[from] serde_yaml::Error),
}
