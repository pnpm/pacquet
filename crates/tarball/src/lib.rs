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

pub async fn download_tarball(url: &str, tarball_path: &Path) -> Result<(), TarballError> {
    let mut stream = reqwest::get(url).await?.bytes_stream();
    let mut file = File::create(tarball_path)?;

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(TarballError::Network)?;
        file.write_all(&chunk)?;
    }

    Ok(())
}

pub fn extract_tarball(tarball_path: &Path, extract_path: &Path) -> Result<(), TarballError> {
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

pub async fn download_direct_dependency(
    name: &str,
    version: &str,
    url: &str,
    node_modules_path: &Path,
    store_path: &Path,
    // For example: fastify@1.1.0
    // For dependencies of fastify: fastify@1.1.0/node_modules/fastify
    package_identifier: &str,
) -> Result<(), TarballError> {
    let store_folder_name = format!("{0}@{version}", normalize(name));

    let tarball_path = store_path.join(format!("{store_folder_name}.tar.gz"));
    let package_path =
        store_path.join(normalize(package_identifier)).join("node_modules").join(name);
    let package_node_modules_folder_path = node_modules_path.join(name);

    // If name contains `/` such as @fastify/error, we need to make sure that @fastify folder
    // exists before we symlink to that directory.
    if name.contains('/') {
        fs::create_dir_all(package_node_modules_folder_path.parent().unwrap())?;
    }

    // Do not try to install dependency if this version already exists in package.json
    if package_path.exists() {
        // Package might be installed into the virtual store, but not symlinked.
        if !package_node_modules_folder_path.exists() {
            symlink_dir(&package_path, &package_node_modules_folder_path)?;
        }
        return Ok(());
    }

    download_tarball(url, &tarball_path).await?;

    fs::create_dir_all(&package_path)?;
    extract_tarball(&tarball_path, &package_path)?;

    // TODO: Currently symlink paths are absolute paths.
    // If you move the root folder to a different path, all symlinks will be broken.
    symlink_dir(&package_path, &package_node_modules_folder_path)?;

    Ok(())
}

pub async fn download_indirect_dependency(
    name: &str,
    version: &str,
    url: &str,
    store_path: &Path,
    symlink_to: &Path,
) -> Result<(), TarballError> {
    let store_folder_name = format!("{0}@{version}", normalize(name));

    let tarball_path = store_path.join(format!("{store_folder_name}.tar.gz"));
    let package_path = store_path.join(&store_folder_name).join("node_modules").join(name);

    // If name contains `/` such as @fastify/error, we need to make sure that @fastify folder
    // exists before we symlink to that directory.
    if name.contains('/') {
        fs::create_dir_all(symlink_to.parent().unwrap())?;
    }

    // Do not try to install dependency if this version already exists in package.json
    if store_path.join(&store_folder_name).exists() {
        symlink_dir(&package_path, &symlink_to.to_path_buf())?;
        return Ok(());
    }

    download_tarball(url, &tarball_path).await?;

    fs::create_dir_all(&package_path)?;
    extract_tarball(&tarball_path, &package_path)?;

    // TODO: Currently symlink paths are absolute paths.
    // If you move the root folder to a different path, all symlinks will be broken.
    symlink_dir(&package_path, &symlink_to.to_path_buf())?;

    Ok(())
}
