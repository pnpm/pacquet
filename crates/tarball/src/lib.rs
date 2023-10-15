use std::{
    collections::HashMap,
    ffi::OsString,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pipe_trait::Pipe;
use reqwest::Client;
use ssri::{Integrity, IntegrityChecker};
use tar::Archive;
use tokio::sync::{Notify, RwLock};
use tracing::instrument;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

#[derive(Debug, Display, Error, Diagnostic)]
#[display(fmt = "Failed to fetch {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    pub error: reqwest::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display(fmt = "Cannot parse {integrity:?} from {url} as an integrity: {error}")]
pub struct ParseIntegrityError {
    pub url: String,
    pub integrity: String,
    #[error(source)]
    pub error: ssri::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display(fmt = "Failed to verify the integrity of {url}: {error}")]
pub struct VerifyChecksumError {
    pub url: String,
    #[error(source)]
    pub error: ssri::Error,
}

#[derive(Debug, Display, Error, From, Diagnostic)]
#[non_exhaustive]
pub enum TarballError {
    #[diagnostic(code(pacquet_tarball::fetch_tarball))]
    FetchTarball(NetworkError),

    #[from(ignore)]
    #[diagnostic(code(pacquet_tarball::io_error))]
    ReadTarballEntries(std::io::Error),

    #[diagnostic(code(pacquet_tarball::parse_integrity_error))]
    ParseIntegrity(ParseIntegrityError),

    #[diagnostic(code(pacquet_tarball::verify_checksum_error))]
    Checksum(VerifyChecksumError),

    #[from(ignore)]
    #[display(fmt = "Integrity creation failed: {_0}")]
    #[diagnostic(code(pacquet_tarball::integrity_error))]
    Integrity(ssri::Error),

    #[from(ignore)]
    #[display(fmt = "Failed to decode gzip: {_0}")]
    #[diagnostic(code(pacquet_tarball::decode_gzip))]
    DecodeGzip(InflateDecodeErrors),

    #[from(ignore)]
    #[display(fmt = "Failed to write cafs: {_0}")]
    #[diagnostic(transparent)]
    WriteCafs(pacquet_cafs::CafsError),

    #[from(ignore)]
    #[diagnostic(code(pacquet_tarball::task_join_error))]
    TaskJoin(tokio::task::JoinError),
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

    DeflateDecoder::new_with_options(gz_data, options)
        .decode_gzip()
        .map_err(TarballError::DecodeGzip)
}

#[instrument(skip(data), fields(data_len = data.len()))]
fn verify_checksum(data: &[u8], integrity: Integrity) -> Result<ssri::Algorithm, ssri::Error> {
    integrity.pipe(IntegrityChecker::new).chain(data).result()
}

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Path to the store directory.
    pub store_dir: &'static Path,
    /// Integrity of the tarball. It can be obtained from the registry index.
    pub package_integrity: &'a str,
    /// Unpack size of the tarball. It can be obtained from the registry index.
    pub package_unpacked_size: Option<usize>,
    /// URL to the tarball.
    pub package_url: &'a str,
}

impl<'a> DownloadTarballToStore<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
        let &DownloadTarballToStore { tarball_cache, package_url, .. } = &self;

        // QUESTION: I see no copying from existing store_dir, is there such mechanism?
        // TODO: If it's not implemented yet, implement it

        if let Some(cache_lock) = tarball_cache.get(package_url) {
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
            if tarball_cache.insert(package_url.to_string(), Arc::clone(&cache_lock)).is_some() {
                tracing::warn!(target: "pacquet::download", ?package_url, "Race condition detected when writing to cache");
            }
            let cas_paths = self.without_cache().await?;
            let mut cache_write = cache_lock.write().await;
            *cache_write = CacheValue::Available(Arc::clone(&cas_paths));
            notify.notify_waiters();
            Ok(cas_paths)
        }
    }

    async fn without_cache(&self) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
        let &DownloadTarballToStore {
            http_client,
            store_dir,
            package_integrity,
            package_unpacked_size,
            package_url,
            ..
        } = self;

        tracing::info!(target: "pacquet::download", ?package_url, "New cache");

        let network_error = |error| {
            TarballError::FetchTarball(NetworkError { url: package_url.to_string(), error })
        };
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
            let data =
                decompress_gzip(&response, package_unpacked_size).map_err(TaskError::Other)?;
            Archive::new(Cursor::new(data))
                .entries()
                .map_err(TarballError::ReadTarballEntries)
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
                    let integrity = pacquet_cafs::write_sync(store_dir, &buffer)
                        .map_err(TarballError::WriteCafs)?;

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
        let cas_files = DownloadTarballToStore {
            tarball_cache: &Default::default(),
            http_client: &Default::default(),
            store_dir: store_path,
            package_integrity: "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz"
        }
        .run()
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
        DownloadTarballToStore {
            tarball_cache: &Default::default(),
            http_client: &Default::default(),
            store_dir: store_path,
            package_integrity: "sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==",
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        }
        .run()
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }
}
