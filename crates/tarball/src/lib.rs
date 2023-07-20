#![feature(error_generic_member_access, provide_any)]

mod symlink;

use std::{
    backtrace::Backtrace,
    env,
    fs::{self},
    io::Cursor,
    path::PathBuf,
};

use libdeflater::DecompressionError;
use ssri::{Algorithm, IntegrityOpts};
use tar::Archive;
use thiserror::Error;
use tracing::{event, instrument, Level};
use uuid::Uuid;

use crate::symlink::symlink_dir;

#[derive(Error, Debug)]
pub enum TarballError {
    #[error("network error while downloading {source}")]
    Network {
        #[from]
        source: reqwest::Error,
        backtrace: Backtrace,
    },
    #[error("io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
        backtrace: Backtrace,
    },
    #[error("checksum mismatch. provided {provided} should match {expected}")]
    ChecksumMismatch { provided: String, expected: String },
    #[error("decompression error: {source}")]
    Decompression {
        #[from]
        source: DecompressionError,
        backtrace: Backtrace,
    },
}

#[derive(Debug)]
pub struct TarballManager {
    http_client: reqwest::Client,
}

impl Default for TarballManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TarballManager {
    pub fn new() -> Self {
        TarballManager { http_client: reqwest::Client::new() }
    }

    #[instrument]
    fn extract(&self, data: Vec<u8>, extract_path: &PathBuf) -> Result<(), TarballError> {
        let unpack_path = env::temp_dir().join(Uuid::new_v4().to_string());
        event!(Level::DEBUG, "unpacking tarball to {}", unpack_path.display());
        let mut archive = Archive::new(Cursor::new(data));
        archive.unpack(&unpack_path)?;
        fs::create_dir_all(extract_path)?;
        fs::rename(unpack_path.join("package"), extract_path)?;
        fs::remove_dir_all(&unpack_path)?;
        Ok(())
    }

    #[instrument]
    fn verify_checksum(&self, data: &bytes::Bytes, integrity: &str) -> Result<(), TarballError> {
        let expected = if integrity.starts_with("sha1-") {
            let hash = IntegrityOpts::new().algorithm(Algorithm::Sha1).chain(data).result();
            format!("sha1-{}", hash.to_hex().1)
        } else {
            IntegrityOpts::new().algorithm(Algorithm::Sha512).chain(data).result().to_string()
        };

        if integrity != expected {
            Err(TarballError::ChecksumMismatch { provided: integrity.to_string(), expected })
        } else {
            Ok(())
        }
    }

    #[instrument]
    fn decompress_gzip(&self, gz_data: &[u8]) -> Result<Vec<u8>, TarballError> {
        // gzip RFC1952: a valid gzip file has an ISIZE field in the
        // footer, which is a little-endian u32 number representing the
        // decompressed size. This is ideal for libdeflate, which needs
        // preallocating the decompressed buffer.
        let isize = {
            let isize_start = gz_data.len() - 4;
            let isize_bytes: [u8; 4] = gz_data[isize_start..].try_into().unwrap();
            u32::from_le_bytes(isize_bytes) as usize
        };

        let mut decompressor = libdeflater::Decompressor::new();

        let mut outbuf = vec![0; isize];
        decompressor.gzip_decompress(gz_data, &mut outbuf)?;
        Ok(outbuf)
    }

    #[instrument]
    pub async fn download_dependency(
        &self,
        integrity: &str,
        url: &str,
        save_path: &PathBuf,
        symlink_to: &PathBuf,
    ) -> Result<(), TarballError> {
        let symlink_exists = symlink_to.is_symlink();

        // If name contains `/` such as @fastify/error, we need to make sure that @fastify folder
        // exists before we symlink to that directory.
        if let Some(parent_folder) = symlink_to.parent() {
            fs::create_dir_all(parent_folder)?;
        }

        // Do not try to install dependency if this version already exists in package.json
        if save_path.exists() || symlink_exists {
            if !symlink_exists {
                symlink_dir(&save_path, &symlink_to)?;
            }
            return Ok(());
        }

        let response = self.http_client.get(url).send().await?.bytes().await?;
        self.verify_checksum(&response, integrity)?;
        let data = self.decompress_gzip(&response)?;
        self.extract(data, save_path)?;

        // TODO: Currently symlink paths are absolute paths.
        // If you move the root folder to a different path, all symlinks will be broken.
        symlink_dir(&save_path, &symlink_to)?;

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
        env::set_current_dir(&parent_folder).unwrap();
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
        let current_path = env::current_dir().unwrap();
        let parent_folder = create_folders();
        let store_path = parent_folder.join("store");
        let node_modules_path = parent_folder.join("node_modules");
        let save_path = store_path.join("@fastify+error@3.3.0");
        let symlink_path = node_modules_path.join("@fastify/error");

        let manager = TarballManager::new();

        manager
            .download_dependency(
                "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
                "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
                &save_path,
                &symlink_path,
            )
            .await
            .unwrap();

        // Validate if we delete the tar.gz file
        assert!(!store_path.join("@fastify+error@3.3.0.tar.gz").exists());
        // Make sure we create store path with normalized name
        assert!(save_path.as_path().is_dir());
        assert!(save_path.join("package.json").is_file());
        // Make sure we create a symlink on node_modules folder
        assert!(symlink_path.exists());
        assert!(symlink_path.is_symlink());
        // Make sure the symlink is looking to the correct place
        assert_eq!(fs::read_link(&symlink_path).unwrap(), save_path);
        //Make sure we create a @fastify folder inside node_modules
        assert!(node_modules_path.join("@fastify").is_dir());
        assert!(node_modules_path.join("@fastify/error").is_symlink());
        assert!(node_modules_path.join("@fastify/error/package.json").is_file());

        env::set_current_dir(current_path).unwrap();
        fs::remove_dir_all(&parent_folder).unwrap();
    }
}
