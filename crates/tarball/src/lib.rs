use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    str::FromStr,
};

use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
    tracing::{self, instrument},
};
use reqwest::Client;
use ssri::{Integrity, IntegrityChecker};
use tar::Archive;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

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

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::task_join_error))]
    TaskJoin(#[from] tokio::task::JoinError),
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
pub async fn download_tarball_to_store(
    store_dir: &Path,
    package_integrity: &str,
    package_unpacked_size: Option<usize>,
    package_url: &str,
) -> Result<HashMap<String, PathBuf>, TarballError> {
    let http_client = Client::new();
    let response = http_client.get(package_url).send().await?.bytes().await?;

    let store_dir = store_dir.to_path_buf(); // TODO: use Arc
    let package_integrity = package_integrity.to_string(); // TODO: use Arc
    tokio::task::spawn_blocking(move || {
        verify_checksum(&response, &package_integrity)?;
        let data = decompress_gzip(&response, package_unpacked_size)?;
        let mut archive = Archive::new(Cursor::new(data));
        let cas_files = archive
            .entries()?
            .map(|entry| -> Result<(String, PathBuf), TarballError> {
                let mut entry = entry.unwrap();

                // Read the contents of the entry
                let mut buffer = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buffer).unwrap();

                let entry_path = entry.path().unwrap();
                let cleaned_entry_path = entry_path.components().skip(1).collect::<PathBuf>(); // QUESTION: why not collect Vec instead?
                let integrity = pacquet_cafs::write_sync(&store_dir, &buffer)?;

                Ok((
                    cleaned_entry_path.to_str().expect("invalid UTF-8").to_string(),
                    store_dir.join(integrity),
                ))
            })
            .collect::<Result<HashMap<String, PathBuf>, TarballError>>()?;

        Ok::<_, TarballError>(cas_files)
    })
    .await
    .expect("no join error")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn packages_under_orgs_should_work() {
        let store_path = tempdir().unwrap();
        let cas_files = download_tarball_to_store(
            store_path.path(),
            "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            Some(16697),
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
        download_tarball_to_store(
            store_path.path(),
            "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            Some(16697),
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        )
        .await
        .expect_err("checksum mismatch");
    }
}
