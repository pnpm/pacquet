pub mod error;
mod symlink;

use std::{
    env,
    fs::{self, File},
    io::Write,
    path::Path,
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use tar::Archive;
use uuid::Uuid;

use crate::{error::TarballError, symlink::symlink_dir};

pub fn normalize(input: &str) -> String {
    input.replace('/', "+")
}

async fn download_tarball(url: &str, tarball_path: &Path) -> Result<(), TarballError> {
    let mut stream = reqwest::get(url).await?.bytes_stream();
    let mut file = File::create(tarball_path)?;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(TarballError::Network)?;
        file.write_all(&chunk).map_err(TarballError::Io)?;
    }

    Ok(())
}

fn extract_tarball(tarball_path: &Path, extract_path: &Path) -> Result<(), TarballError> {
    let id = Uuid::new_v4();
    let unpack_path = env::temp_dir().join(id.to_string());
    fs::create_dir_all(&unpack_path)?;
    let tar_gz = File::open(tarball_path)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(&unpack_path)?;
    fs::remove_file(tarball_path)?;
    fs::rename(unpack_path.join("package"), extract_path)?;
    fs::remove_dir_all(&unpack_path)?;
    Ok(())
}

pub async fn download_and_extract(
    unsanitized_name: &str,
    version: &str,
    url: &str,
    store_path: &Path,
    node_modules: &Path,
    should_symlink: bool,
    // For example: fastify@1.1.0
    // For dependencies of fastify: fastify@1.1.0/node_modules/fastify
    package_identifier: &str,
) -> Result<(), TarballError> {
    let name = normalize(unsanitized_name);
    let tarball_path = store_path.join(format!("{name}@{version}.tar.gz"));
    download_tarball(url, &tarball_path).await?;

    let package_path = store_path
        .join(format!("{0}/node_modules/{unsanitized_name}", normalize(package_identifier)));
    fs::create_dir_all(&package_path)?;
    extract_tarball(&tarball_path, &package_path)?;

    let node_modules_path = node_modules.join(unsanitized_name);

    if !node_modules_path.exists() && should_symlink {
        // TODO: Installing @fastify/error fails because of missing @fastify folder.
        // TODO: Currently symlink paths are absolute paths.
        // If you move the root folder to a different path, all symlinks will be broken.
        symlink_dir(&package_path, &node_modules_path)?;
    }

    Ok(())
}
