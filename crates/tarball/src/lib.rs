use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::DashMap;
use pacquet_diagnostics::{
    miette::{self, Diagnostic},
    thiserror::{self, Error},
    tracing::{self, instrument},
};
use pipe_trait::Pipe;
use reqwest::Client;
use ssri::{Integrity, IntegrityChecker};
use tar::Archive;
use tokio::sync::RwLock;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

#[derive(Error, Debug, Diagnostic)]
#[error("Failed to fetch {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    pub error: reqwest::Error,
}

#[derive(Error, Debug, Diagnostic)]
#[error("Cannot parse {integrity:?} from {url} as an integrity: {error}")]
pub struct ParseIntegrityError {
    pub url: String,
    pub integrity: String,
    #[source]
    pub error: ssri::Error,
}

#[derive(Error, Debug, Diagnostic)]
#[error("Failed to verify the integrity of {url}: {error}")]
pub struct VerifyChecksumError {
    pub url: String,
    #[source]
    pub error: ssri::Error,
}

#[derive(Error, Debug, Diagnostic)]
#[non_exhaustive]
pub enum TarballError {
    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::request_error))]
    Network(#[from] NetworkError),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::io_error))]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::parse_integrity_error))]
    ParseIntegrity(#[from] ParseIntegrityError),

    #[error(transparent)]
    #[diagnostic(code(pacquet_tarball::verify_checksum_error))]
    Checksum(#[from] VerifyChecksumError),

    #[error("integrity creation failed: {}", _0)]
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

/// Value of the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheValue {
    /// The package is being processed.
    InProgress,
    /// The package is saved.
    Available(Arc<HashMap<String, PathBuf>>),
}

/// Internal cache of tarballs.
///
/// The key of this hashmap is the url of each tarball.
pub type Cache = DashMap<String, Arc<RwLock<CacheValue>>>;

#[instrument(skip(gz_data), fields(gz_data_len = gz_data.len()))]
fn decompress_gzip(gz_data: &[u8], unpacked_size: Option<usize>) -> Result<Vec<u8>, TarballError> {
    let mut options = DeflateOptions::default().set_confirm_checksum(false);

    if let Some(size) = unpacked_size {
        options = options.set_size_hint(size);
    }

    let mut decoder = DeflateDecoder::new_with_options(gz_data, options);
    let decompressed = decoder.decode_gzip()?;

    Ok(decompressed)
}

#[instrument(skip(data), fields(data_len = data.len()))]
fn verify_checksum(data: &[u8], integrity: Integrity) -> Result<ssri::Algorithm, ssri::Error> {
    integrity.pipe(IntegrityChecker::new).chain(data).result()
}

#[instrument]
pub async fn download_tarball_to_store(
    cache: &Cache,
    store_dir: &Path,
    package_integrity: &str,
    package_unpacked_size: Option<usize>,
    package_url: &str,
) -> Result<Arc<HashMap<String, PathBuf>>, TarballError> {
    if let Some(cache_lock) = cache.get(package_url) {
        tracing::info!(target: "pacquet::download", ?package_url, "Cache hit");

        loop {
            match &*cache_lock.read().await {
                CacheValue::InProgress => continue,
                CacheValue::Available(cas_paths) => return Ok(cas_paths.clone()),
            }
        }
    }

    tracing::info!(target: "pacquet::download", ?package_url, "Cache miss");

    let cache_lock = CacheValue::InProgress.pipe(RwLock::new).pipe(Arc::new);
    if cache.insert(package_url.to_string(), cache_lock.clone()).is_some() {
        tracing::warn!(target: "pacquet::download", ?package_url, "Race condition detected when writing to cache");
    }

    let network_error = |error| NetworkError { url: package_url.to_string(), error };
    let response = Client::new()
        .get(package_url)
        .send()
        .await
        .map_err(network_error)?
        .bytes()
        .await
        .map_err(network_error)?;

    tracing::info!(target: "pacquet::download", ?package_url, "Downloaded completed");

    // TODO: benchmark and profile to see if spawning is actually necessary
    let store_dir = store_dir.to_path_buf(); // TODO: use Arc
    let package_integrity: Integrity =
        package_integrity.parse().map_err(|error| ParseIntegrityError {
            url: package_url.to_string(),
            integrity: package_integrity.to_string(),
            error,
        })?;
    let url = package_url.to_string(); // TODO: use Arc
    let cas_paths = tokio::task::spawn_blocking(move || {
        verify_checksum(&response, package_integrity)
            .map_err(|error| VerifyChecksumError { url, error })?;
        let data = decompress_gzip(&response, package_unpacked_size)?;
        Archive::new(Cursor::new(data))
            .entries()?
            .filter(|entry| !entry.as_ref().unwrap().header().entry_type().is_dir())
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
            .collect::<Result<HashMap<String, PathBuf>, TarballError>>()
    })
    .await
    .expect("no join error")?
    .pipe(Arc::new);

    tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

    let mut cache_write = cache_lock.write().await;
    *cache_write = CacheValue::Available(cas_paths.clone());

    Ok(cas_paths)
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
            &Default::default(),
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
            &Default::default(),
            store_path.path(),
            "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            Some(16697),
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        )
        .await
        .expect_err("checksum mismatch");
    }
}
