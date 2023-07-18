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
use tracing::{event, instrument, Level};
use uuid::Uuid;

use crate::symlink::symlink_dir;

#[derive(Error, Debug)]
pub enum TarballError {
    #[error("network error while downloading {0}")]
    Network(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
pub struct TarballManager;

impl Default for TarballManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TarballManager {
    pub fn new() -> Self {
        TarballManager {}
    }

    #[instrument]
    async fn download(&self, url: &str, tarball_path: &Path) -> Result<(), TarballError> {
        let mut stream = reqwest::get(url).await?.bytes_stream();
        let mut file = File::create(tarball_path)?;
        event!(Level::DEBUG, "downloading tarball to {}", tarball_path.display());

        while let Some(item) = stream.next().await {
            let chunk = item.map_err(TarballError::Network)?;
            file.write_all(&chunk)?;
        }

        Ok(())
    }

    #[instrument]
    fn extract(&self, tarball_path: &Path, extract_path: &Path) -> Result<(), TarballError> {
        let unpack_path = env::temp_dir().join(Uuid::new_v4().to_string());
        event!(Level::DEBUG, "unpacking tarball to {}", unpack_path.display());
        let tar_gz = File::open(tarball_path)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        archive.unpack(&unpack_path)?;
        fs::remove_file(tarball_path)?;
        fs::create_dir_all(extract_path)?;
        fs::rename(unpack_path.join("package"), extract_path)?;
        fs::remove_dir_all(&unpack_path)?;
        Ok(())
    }

    #[instrument]
    pub async fn download_dependency(
        &self,
        name: &str,
        url: &str,
        save_path: &Path,
        symlink_to: &Path,
    ) -> Result<(), TarballError> {
        let symlink_exists = symlink_to.is_symlink();

        // If name contains `/` such as @fastify/error, we need to make sure that @fastify folder
        // exists before we symlink to that directory.
        if name.contains('/') && !symlink_exists {
            fs::create_dir_all(symlink_to.parent().unwrap())?;
        }

        // Do not try to install dependency if this version already exists in package.json
        if save_path.exists() || symlink_exists {
            if !symlink_exists {
                symlink_dir(&save_path.to_path_buf(), &symlink_to.to_path_buf())?;
            }
            return Ok(());
        }

        let tarball_path = env::temp_dir().join(Uuid::new_v4().to_string());
        self.download(url, &tarball_path).await?;

        self.extract(&tarball_path, save_path)?;

        // TODO: Currently symlink paths are absolute paths.
        // If you move the root folder to a different path, all symlinks will be broken.
        symlink_dir(&save_path.to_path_buf(), &symlink_to.to_path_buf())?;

        Ok(())
    }
}

pub fn get_package_store_folder_name(input: &str, version: &str) -> String {
    format!("{0}@{1}", input.replace('/', "+"), version)
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

    #[test]
    fn generate_correct_package_name() {
        assert_eq!(
            get_package_store_folder_name("@fastify/error", "3.3.0"),
            "@fastify+error@3.3.0"
        );
        assert_eq!(
            get_package_store_folder_name("fast-querystring", "1.1.0"),
            "fast-querystring@1.1.0"
        );
    }

    #[tokio::test]
    async fn packages_under_orgs_should_work() {
        let parent_folder = create_folders();
        let store_path = parent_folder.join("store");
        let node_modules_path = parent_folder.join("node_modules");
        let save_path = store_path.join("@fastify+error@3.3.0");
        let symlink_path = node_modules_path.join("@fastify/error");

        let manager = TarballManager::new();

        manager
            .download_dependency(
                "@fastify/error",
                "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
                &save_path.to_path_buf(),
                &symlink_path.to_path_buf(),
            )
            .await
            .unwrap();

        // Validate if we delete the tar.gz file
        assert!(!store_path.join("@fastify+error@3.3.0.tar.gz").exists());
        // Make sure we create store path with normalized name
        assert!(store_path.join("@fastify+error@3.3.0").is_dir());
        // Make sure we create a symlink on node_modules folder
        assert!(symlink_path.exists());
        assert!(symlink_path.is_symlink());
        //Make sure we create a @fastify folder inside node_modules
        assert!(node_modules_path.join("@fastify").is_dir());

        fs::remove_dir_all(&parent_folder).unwrap();
    }
}
