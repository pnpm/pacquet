pub mod error;
mod symlink;

use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tar::Archive;

use crate::{error::TarballError, symlink::symlink_dir};

pub async fn download_and_extract(
    name: &str,
    version: &str,
    url: &str,
    store_path: &Path,
    node_modules: &Path,
    should_symlink: bool,
) -> Result<(), TarballError> {
    // Place to save `.tar.gz` file
    // For now: node_modules/".pacquet/fast-querystring@1.0.0.tar.gz"
    let tarball_location = store_path.join(format!("{name}@{version}.tar.gz"));
    // Place to extract the contents of the `.tar.gz` file
    // For now: node_modules/.pacquet/fast-querystring@1.0.0
    let tarball_extract_location = store_path.join(format!("_{name}@{version}"));

    let mut stream = reqwest::get(url).await?.bytes_stream();
    let mut file = File::create(&tarball_location)?;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(TarballError::Network)?;
        file.write_all(&chunk).map_err(TarballError::Io)?;
    }

    let tar_gz = File::open(&tarball_location)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(&tarball_extract_location)?;
    fs::remove_file(&tarball_location)?;

    // Tarball contains the source code of the package inside a "package" folder
    // We need to move the contents of this folder to the appropriate node_modules folder.
    let package_folder = tarball_extract_location.join("package");
    let package_store_path = store_path.join(format!("{name}@{version}"));
    let node_modules_path = node_modules.join(name);

    if !node_modules_path.exists() {
        fs::rename(package_folder, &package_store_path)?;
        if should_symlink {
            symlink_dir(&package_store_path, &node_modules_path)?;
        }
    }

    fs::remove_dir_all(&tarball_extract_location)?;

    Ok(())
}
