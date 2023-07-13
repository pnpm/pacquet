use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tar::Archive;
use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TarballError {
    #[error("network error while downloading `${0}`")]
    Network(reqwest::Error),
    #[error("io error: `{0}`")]
    Io(io::Error),
}

impl From<io::Error> for TarballError {
    fn from(value: io::Error) -> Self {
        TarballError::Io(value)
    }
}

impl From<reqwest::Error> for TarballError {
    fn from(value: reqwest::Error) -> Self {
        TarballError::Network(value)
    }
}

pub async fn download_and_extract(
    name: &str,
    version: &str,
    url: &str,
    cache_directory: &Path,
    node_modules: &Path,
) -> Result<(), TarballError> {
    // Place to save `.tar.gz` file
    // For now: ".pacquet/fast-querystring@1.0.0.tar.gz"
    let tarball_location = cache_directory.join(format!("{name}@{version}"));
    // Place to extract the contents of the `.tar.gz` file
    // For now: .pacquet/fast-querystring/1.0.0
    let extract_location = cache_directory.join(name).join(version);

    let mut stream = reqwest::get(url).await.map_err(TarballError::Network)?.bytes_stream();

    let mut file = File::create(&tarball_location).map_err(TarballError::Io)?;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(TarballError::Network)?;
        file.write_all(&chunk).map_err(TarballError::Io)?;
    }

    let tar_gz = File::open(&tarball_location)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(&extract_location)?;

    std::fs::remove_file(&tarball_location)?;

    // Tarball contains the source code of the package inside a "package" folder
    // We need to move the contents of this folder to the appropriate node_modules folder.
    let package_folder = extract_location.join("package");

    if package_folder.exists() {
        let node_modules_path = node_modules.to_owned().join(name);

        if !node_modules_path.exists() {
            fs::rename(&package_folder, &node_modules_path)?;
        }

        fs::remove_dir_all(cache_directory.join(name))?;
    }

    Ok(())
}
