use thiserror::Error;

#[derive(Error, Debug)]
pub enum TarballError {
    #[error("network error while downloading {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
