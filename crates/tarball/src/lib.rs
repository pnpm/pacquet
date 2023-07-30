#![feature(error_generic_member_access, provide_any)]

use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
};

use libdeflater::{DecompressionError, Decompressor};
use miette::Diagnostic;
use ssri::{Algorithm, IntegrityOpts};
use tar::Archive;
use thiserror::Error;
use tracing::instrument;

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum TarballError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::network_error))]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::io_error))]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch. provided {provided} should match {expected}")]
    #[diagnostic(code(pacquet_tarball::checksum_mismatch_error))]
    ChecksumMismatch { provided: String, expected: String },

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::decompression_error))]
    Decompression(#[from] DecompressionError),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Cafs(#[from] pacquet_cafs::CafsError),
}

#[instrument]
fn decompress_gzip(gz_data: &[u8]) -> Result<Vec<u8>, TarballError> {
    // gzip RFC1952: a valid gzip file has an ISIZE field in the
    // footer, which is a little-endian u32 number representing the
    // decompressed size. This is ideal for lib-deflate, which needs
    // pre-allocating the decompressed buffer.
    let isize = {
        let isize_start = gz_data.len() - 4;
        let isize_bytes: [u8; 4] = gz_data[isize_start..].try_into().unwrap();
        u32::from_le_bytes(isize_bytes) as usize
    };

    let mut decompressor = Decompressor::new();
    let mut outbuf = vec![0; isize];
    decompressor.gzip_decompress(gz_data, &mut outbuf)?;
    Ok(outbuf)
}

#[instrument]
fn verify_checksum(data: &bytes::Bytes, integrity: &str) -> Result<(), TarballError> {
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

#[derive(Debug)]
pub struct TarballManager {
    http_client: Box<reqwest::Client>,
    store_dir: PathBuf,
}

impl TarballManager {
    pub fn new<P: Into<PathBuf>>(store_dir: P) -> Self {
        TarballManager {
            http_client: Box::new(reqwest::Client::new()),
            store_dir: store_dir.into(),
        }
    }

    #[instrument]
    pub async fn download_dependency(
        &self,
        integrity: &str,
        url: &str,
    ) -> Result<HashMap<String, Vec<u8>>, TarballError> {
        let mut cas_files = HashMap::<String, Vec<u8>>::new();

        let response = self.http_client.get(url).send().await?.bytes().await?;
        verify_checksum(&response, integrity)?;
        let data = decompress_gzip(&response)?;
        let mut archive = Archive::new(Cursor::new(data));

        for entry in archive.entries()? {
            let mut entry = entry?;

            // Read the contents of the entry
            let mut buffer = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buffer)?;

            let entry_path = entry.path()?;
            let cleaned_entry_path = entry_path.components().skip(1).collect::<PathBuf>();
            pacquet_cafs::write_sync(&self.store_dir, &buffer)?;

            cas_files.insert(cleaned_entry_path.to_str().unwrap().to_string(), buffer);
        }

        Ok(cas_files)
    }
}

pub fn get_package_store_folder_name(input: &str, version: &str) -> String {
    format!("{0}@{1}", input.replace('/', "+"), version)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

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
    #[cfg(not(target_os = "windows"))]
    async fn packages_under_orgs_should_work() {
        let store_path = tempdir().unwrap();
        let manager = TarballManager::new(store_path.path());

        let cas_files = manager.download_dependency(
            "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        ).await.unwrap();

        let mut filenames = cas_files.keys().collect::<Vec<_>>();
        filenames.sort();
        assert_eq!(
            filenames,
            vec![
                ".github/dependabot.yml",
                ".github/workflows/ci.yml",
                ".taprc",
                "LICENSE",
                "README.md",
                "benchmarks/create.js",
                "benchmarks/instantiate.js",
                "benchmarks/no-stack.js",
                "benchmarks/toString.js",
                "index.js",
                "package.json",
                "test/index.test.js",
                "types/index.d.ts",
                "types/index.test-d.ts"
            ]
        );
    }

    #[tokio::test]
    async fn should_throw_error_on_checksum_mismatch() {
        let store_path = tempdir().unwrap();

        // Try calling default as well
        TarballManager::new(store_path.path())
            .download_dependency(
                "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
                "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            )
            .await
            .expect_err("checksum mismatch");
    }
}
