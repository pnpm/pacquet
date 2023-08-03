use std::path::Path;
use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
    str::FromStr,
};

use miette::Diagnostic;
use pacquet_registry::package_version::PackageVersion;
use reqwest::Client;
use ssri::{Integrity, IntegrityChecker};
use tar::Archive;
use thiserror::Error;
use tracing::instrument;
use zune_inflate::errors::InflateDecodeErrors;
use zune_inflate::{DeflateDecoder, DeflateOptions};

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum TarballError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::network_error))]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::io_error))]
    Io(#[from] std::io::Error),

    #[error("checksum mismatch")]
    #[diagnostic(code(pacquet_tarball::checksum_mismatch_error))]
    ChecksumMismatch,

    #[error("integrity creation failed")]
    #[diagnostic(code(pacquet_tarball::integrity_error))]
    Integrity(#[from] ssri::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::decompression_error))]
    Decompression(#[from] InflateDecodeErrors),

    #[error(transparent)]
    #[diagnostic(transparent)]
    Cafs(#[from] pacquet_cafs::CafsError),
}

#[instrument]
fn decompress_gzip(gz_data: &[u8], unpacked_size: Option<usize>) -> Result<Vec<u8>, TarballError> {
    let mut options = DeflateOptions::default().set_confirm_checksum(false);

    if let Some(size) = unpacked_size {
        options = options.set_size_hint(size);
    }

    let mut decoder = DeflateDecoder::new_with_options(gz_data, options);
    let decompressed = decoder.decode_gzip()?;

    Ok(decompressed)
}

#[instrument]
fn verify_checksum(data: &[u8], integrity: &str) -> Result<(), TarballError> {
    let validation = IntegrityChecker::new(Integrity::from_str(integrity)?).chain(data).result();

    if validation.is_err() {
        Err(TarballError::ChecksumMismatch)
    } else {
        Ok(())
    }
}

// #[instrument]
pub async fn download_tarball_to_store<P: AsRef<Path>>(
    http_client: &Client,
    store_dir: P,
    package_version: &PackageVersion,
    url: &str,
) -> Result<HashMap<String, PathBuf>, TarballError> {
    let response = http_client.get(url).send().await?.bytes().await?;
    verify_checksum(&response, &package_version.dist.integrity)?;
    let data = decompress_gzip(&response, package_version.dist.unpacked_size)?;
    let mut archive = Archive::new(Cursor::new(data));

    let cas_files = archive
        .entries()?
        .map(|entry| {
            let mut entry = entry.unwrap();

            // Read the contents of the entry
            let mut buffer = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buffer).unwrap();

            let entry_path = entry.path().unwrap();
            let cleaned_entry_path = entry_path.components().skip(1).collect::<PathBuf>();
            let integrity = pacquet_cafs::write_sync(store_dir.as_ref(), &buffer).unwrap();

            (cleaned_entry_path.to_string_lossy().to_string(), store_dir.as_ref().join(integrity))
        })
        .collect::<HashMap<String, PathBuf>>();

    Ok(cas_files)
}

pub fn get_package_store_folder_name(input: &str, version: &str) -> String {
    format!("{0}@{1}", input.replace('/', "+"), version)
}

#[cfg(test)]
mod tests {
    use node_semver::Version;
    use pacquet_registry::package_distribution::PackageDistribution;
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
        let http_client = reqwest::Client::new();

        let package_version = PackageVersion {
            name: "".to_string(),
            version: Version {
                major: 3,
                minor: 3,
                patch: 0,
                build: vec![],
                pre_release: vec![],
            },            dist: PackageDistribution {
                integrity: "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==".to_string(),
                npm_signature: None,
                shasum: "".to_string(),
                tarball: "".to_string(),
                file_count: None,
                unpacked_size: Some(16697),
            },
            dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
        };
        let cas_files = download_tarball_to_store(
            &http_client,
            store_path.path(),
            &package_version,
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        )
        .await
        .unwrap();

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
        let http_client = Client::new();
        let package_version = PackageVersion {
            name: "".to_string(),
            version: Version {
                major: 3,
                minor: 3,
                patch: 0,
                build: vec![],
                pre_release: vec![],
            },
            dist: PackageDistribution {
                integrity: "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==".to_string(),
                npm_signature: None,
                shasum: "".to_string(),
                tarball: "".to_string(),
                file_count: None,
                unpacked_size: Some(16697),
            },
            dependencies: None,
            dev_dependencies: None,
            peer_dependencies: None,
        };

        download_tarball_to_store(
            &http_client,
            store_path.path(),
            &package_version,
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        )
        .await
        .expect_err("checksum mismatch");
    }
}
