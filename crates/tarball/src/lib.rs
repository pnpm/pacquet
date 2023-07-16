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
use thiserror::Error;
use uuid::Uuid;

use crate::symlink::symlink_dir;

#[derive(Error, Debug)]
pub enum TarballError {
    #[error("network error while downloading {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn create_folders() -> PathBuf {
        let id = Uuid::new_v4();
        let parent_folder = env::temp_dir().join(id.to_string());
        fs::create_dir_all(parent_folder.join("store")).expect("failed to create folder");
        fs::create_dir_all(parent_folder.join("node_modules")).expect("failed to create folder");
        parent_folder
    }

    #[tokio::test]
    async fn ensure_organization_packages_work_as_indirect_dependency() {
        let parent_folder = create_folders();
        let store_path = parent_folder.join("store");
        let node_modules_path = parent_folder.join("node_modules");
        let symlink_path = node_modules_path.join("@fastify/error");

        download_indirect_dependency(
            "@fastify/error",
            "3.3.0",
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            &store_path.to_path_buf(),
            &symlink_path.to_path_buf(),
        )
        .await
        .unwrap();

        // Validate if we delete the tar.gz file
        assert!(!store_path.join(format!("@fastify+error@3.3.0.tar.gz")).exists());
        // Make sure we create store path with normalized name
        assert!(store_path.join("@fastify+error@3.3.0").is_dir());
        // Make sure we create a symlink on node_modules folder
        assert!(symlink_path.exists());
        assert!(symlink_path.is_symlink());
        //Make sure we create a @fastify folder inside node_modules
        assert!(node_modules_path.join("@fastify").is_dir());

        fs::remove_dir_all(&parent_folder).unwrap();
    }

    #[tokio::test]
    async fn do_not_download_existing_indirect_dependency() {
        let parent_folder = create_folders();
        let store_path = parent_folder.join("store");
        let node_modules_path = parent_folder.join("node_modules");
        let symlink_path = node_modules_path.join("@fastify/error");

        // Create a folder to check if we don't download
        fs::create_dir_all(store_path.join("@fastify+error@3.3.0")).unwrap();

        // Deliberately put an invalid URL which fails when trying to download.
        download_indirect_dependency(
            "@fastify/error",
            "3.3.0",
            "https://!!!",
            &store_path.to_path_buf(),
            &symlink_path.to_path_buf(),
        )
        .await
        .unwrap();

        // Make sure we create a symlink on node_modules folder
        assert!(symlink_path.is_symlink());
        //Make sure we create a @fastify folder inside node_modules
        assert!(node_modules_path.join("@fastify").is_dir());

        fs::remove_dir_all(&parent_folder).unwrap();
    }
}
