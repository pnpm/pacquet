use std::{
    collections::HashMap,
    ffi::OsString,
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
use tokio::sync::{Notify, RwLock};
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
#[derive(Debug, Clone)]
pub enum CacheValue {
    /// The package is being processed.
    InProgress(Arc<Notify>),
    /// The package is saved.
    Available(Arc<HashMap<OsString, PathBuf>>),
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

#[instrument(skip(cache), fields(cache_len = cache.len()))]
pub async fn download_tarball_to_store(
    cache: &Cache,
    http_client: &Client,
    store_dir: &'static Path,
    package_integrity: &str,
    package_unpacked_size: Option<usize>,
    package_url: &str,
) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
    if let Some(cache_lock) = cache.get(package_url) {
        let notify = match &*cache_lock.write().await {
            CacheValue::Available(cas_paths) => {
                return Ok(Arc::clone(cas_paths));
            }
            CacheValue::InProgress(notify) => Arc::clone(notify),
        };

        tracing::info!(target: "pacquet::download", ?package_url, "Wait for cache");
        notify.notified().await;
        if let CacheValue::Available(cas_paths) = &*cache_lock.read().await {
            return Ok(Arc::clone(cas_paths));
        }
        unreachable!("Failed to get or compute tarball data for {package_url:?}");
    } else {
        let notify = Arc::new(Notify::new());
        let cache_lock = notify
            .pipe_ref(Arc::clone)
            .pipe(CacheValue::InProgress)
            .pipe(RwLock::new)
            .pipe(Arc::new);
        if cache.insert(package_url.to_string(), Arc::clone(&cache_lock)).is_some() {
            tracing::warn!(target: "pacquet::download", ?package_url, "Race condition detected when writing to cache");
        }
        let cas_paths = download_tarball_to_store_uncached(
            package_url,
            http_client,
            store_dir,
            package_integrity,
            package_unpacked_size,
        )
        .await?;
        let mut cache_write = cache_lock.write().await;
        *cache_write = CacheValue::Available(Arc::clone(&cas_paths));
        notify.notify_waiters();
        Ok(cas_paths)
    }
}

async fn download_tarball_to_store_uncached(
    package_url: &str,
    http_client: &Client,
    store_dir: &'static Path,
    package_integrity: &str,
    package_unpacked_size: Option<usize>,
) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
    tracing::info!(target: "pacquet::download", ?package_url, "New cache");

    let network_error = |error| NetworkError { url: package_url.to_string(), error };
    let response = http_client
        .get(package_url)
        .send()
        .await
        .map_err(network_error)?
        .bytes()
        .await
        .map_err(network_error)?;

    tracing::info!(target: "pacquet::download", ?package_url, "Download completed");

    let package_integrity: Integrity =
        package_integrity.parse().map_err(|error| ParseIntegrityError {
            url: package_url.to_string(),
            integrity: package_integrity.to_string(),
            error,
        })?;
    enum TaskError {
        Checksum(ssri::Error),
        Other(TarballError),
    }
    let cas_paths = tokio::task::spawn(async move {
        verify_checksum(&response, package_integrity).map_err(TaskError::Checksum)?;
        let data = decompress_gzip(&response, package_unpacked_size).map_err(TaskError::Other)?;
        Archive::new(Cursor::new(data))
            .entries()
            .map_err(TarballError::Io)
            .map_err(TaskError::Other)?
            .filter(|entry| !entry.as_ref().unwrap().header().entry_type().is_dir())
            .map(|entry| -> Result<(OsString, PathBuf), TarballError> {
                let mut entry = entry.unwrap();

                // Read the contents of the entry
                let mut buffer = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buffer).unwrap();

                let entry_path = entry.path().unwrap();
                let cleaned_entry_path =
                    entry_path.components().skip(1).collect::<PathBuf>().into_os_string();
                let integrity = pacquet_cafs::write_sync(store_dir, &buffer)?;

                Ok((cleaned_entry_path, store_dir.join(integrity)))
            })
            .collect::<Result<HashMap<OsString, PathBuf>, TarballError>>()
            .map_err(TaskError::Other)
    })
    .await
    .expect("no join error")
    .map_err(|error| match error {
        TaskError::Checksum(error) => {
            TarballError::Checksum(VerifyChecksumError { url: package_url.to_string(), error })
        }
        TaskError::Other(error) => error,
    })?
    .pipe(Arc::new);

    tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

    Ok(cas_paths)
}

#[cfg(test)]
mod tests {
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use tempfile::{tempdir, TempDir};

    use super::*;

    /// **Problem:**
    /// The tested function requires `'static` paths, leaking would prevent
    /// temporary files from being cleaned up.
    ///
    /// **Solution:**
    /// Create [`TempDir`] as a temporary variable (which can be dropped)
    /// but provide its path as `'static`.
    ///
    /// **Side effect:**
    /// The `'static` path becomes dangling outside the scope of [`TempDir`].
    fn tempdir_with_leaked_path() -> (TempDir, &'static Path) {
        let tempdir = tempdir().unwrap();
        let leaked_path = tempdir.path().to_path_buf().pipe(Box::new).pipe(Box::leak);
        (tempdir, leaked_path)
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn packages_under_orgs_should_work() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let cas_files = download_tarball_to_store(
            &Default::default(),
            &Client::new(),
            store_path,
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

        drop(store_dir);
    }

    #[tokio::test]
    async fn should_throw_error_on_checksum_mismatch() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        download_tarball_to_store(
            &Default::default(),
            &Client::new(),
            store_path,
            "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            Some(16697),
            "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        )
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }
}
