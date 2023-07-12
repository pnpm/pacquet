use std::{fs::File, io::Write, path::Path};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tar::Archive;
use thiserror::Error;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum TarballError {
    #[error("network error while downloading `${0}`")]
    Network(String),
    #[error("filesystem error: `{0}`")]
    FileSystem(String),
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
    let tarball_location = cache_directory.join(name.to_owned() + "@" + version);
    // Place to extract the contents of the `.tar.gz` file
    // For now: .pacquet/fast-querystring/1.0.0
    let extract_location = cache_directory.join(name).join(version);

    let mut stream =
        reqwest::get(url).await.or(Err(TarballError::Network(url.to_owned())))?.bytes_stream();

    let mut file = File::create(&tarball_location)
        .or(Err(TarballError::FileSystem("failed to create file".to_owned())))
        .unwrap();

    while let Some(item) = stream.next().await {
        let chunk =
            item.or(Err(TarballError::Network("error while downloading file".to_owned())))?;
        file.write_all(&chunk)
            .or(Err(TarballError::FileSystem("error while writing to file".to_owned())))?;
    }

    let tar_gz = File::open(&tarball_location).unwrap();
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    let _ = archive.unpack(&extract_location);

    std::fs::remove_file(&tarball_location)
        .or(Err(TarballError::FileSystem("removing tarball failed".to_owned())))
        .unwrap();

    // Tarball contains the source code of the package inside a "package" folder
    // We need to move the contents of this folder to the appropriate node_modules folder.
    let package_folder = extract_location.join("package");

    if package_folder.exists() {
        let node_modules_path = node_modules.to_owned().join(name);

        if !node_modules_path.exists() {
            std::fs::rename(&package_folder, &node_modules_path).unwrap();
        }

        std::fs::remove_dir_all(cache_directory.join(name)).unwrap();
    }

    Ok(())
}
