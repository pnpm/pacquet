use std::{
    collections::HashMap,
    ffi::OsString,
    io::{Cursor, Read},
    path::PathBuf,
    sync::Arc,
    time::UNIX_EPOCH,
};

use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine};
use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_fs::file_mode;
use pacquet_store_dir::{
    PackageFileInfo, PackageFilesIndex, StoreDir, WriteCasFileError, WriteTarballIndexFileError,
};
use pipe_trait::Pipe;
use rayon::prelude::*;
use reqwest::Client;
use ssri::Integrity;
use tar::Archive;
use tokio::sync::{Notify, RwLock};
use tracing::instrument;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to fetch {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    pub error: reqwest::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to verify the integrity of {url}: {error}")]
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

    #[diagnostic(code(pacquet_tarball::verify_checksum_error))]
    Checksum(VerifyChecksumError),

    #[from(ignore)]
    #[display("Integrity creation failed: {_0}")]
    #[diagnostic(code(pacquet_tarball::integrity_error))]
    Integrity(ssri::Error),

    #[from(ignore)]
    #[display("Failed to decode gzip: {_0}")]
    #[diagnostic(code(pacquet_tarball::decode_gzip))]
    DecodeGzip(InflateDecodeErrors),

    #[from(ignore)]
    #[display("Failed to write cafs: {_0}")]
    #[diagnostic(transparent)]
    WriteCasFile(WriteCasFileError),

    #[from(ignore)]
    #[display("Failed to write tarball index: {_0}")]
    #[diagnostic(transparent)]
    WriteTarballIndexFile(WriteTarballIndexFileError),

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
fn verify_checksum(data: &[u8], integrity: &Integrity) -> Result<ssri::Algorithm, ssri::Error> {
    integrity.check(data)
}

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    pub http_client: &'a Client,
    pub store_dir: &'static StoreDir,
    pub package_integrity: &'a Integrity,
    pub package_unpacked_size: Option<usize>,
    pub package_url: &'a str,
}

impl<'a> DownloadTarballToStore<'a> {
    /// Execute the subroutine with cache.
    pub async fn with_cache(
        self,
        tarball_cache: &'a Cache,
    ) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
        let &DownloadTarballToStore { package_url, .. } = &self;

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
            let cas_paths = self.without_cache().await?.pipe(Arc::new);
            let mut cache_write = cache_lock.write().await;
            *cache_write = CacheValue::Available(Arc::clone(&cas_paths));
            notify.notify_waiters();
            Ok(cas_paths)
        }
    }

    /// Execute the subroutine without a cache.
    pub async fn without_cache(&self) -> Result<HashMap<OsString, PathBuf>, TarballError> {
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
            .map_err(network_error)?
            .pipe(Arc::new);

        tracing::info!(target: "pacquet::download", ?package_url, "Download completed");

        // TODO: Cloning here is less than desirable, there are 2 possible solutions for this problem:
        // 1. Use an Arc and convert this line to Arc::clone.
        // 2. Replace ssri with base64 and serde magic (which supports Copy).
        let package_integrity = package_integrity.clone().pipe(Arc::new);

        let verify_checksum_task = {
            let response = Arc::clone(&response);
            let package_integrity = Arc::clone(&package_integrity);
            tokio::task::spawn(async move { verify_checksum(&response, &package_integrity) })
        };

        let extract_tarball_task = tokio::task::spawn(async move {
            // TODO: move tarball extraction to its own function
            // TODO: test it
            // TODO: test the duplication of entries

            let mut archive = decompress_gzip(&response, package_unpacked_size)?
                .pipe(Cursor::new)
                .pipe(Archive::new);

            struct FileInfo {
                cleaned_entry_path: OsString,
                file_mode: u32,
                file_size: Option<u64>,
                buffer: Vec<u8>,
            }
            let (cas_paths, index_entries) = archive
                .entries()
                .map_err(TarballError::ReadTarballEntries)?
                .filter(|entry| !entry.as_ref().unwrap().header().entry_type().is_dir())
                .map(|entry| entry.expect("get entry"))
                .map(|mut entry| {
                    let cleaned_entry_path = entry
                        .path()
                        .expect("get path") // TODO: properly propagate this error
                        .components()
                        .skip(1)
                        .collect::<PathBuf>()
                        .into_os_string();
                    let file_mode = entry.header().mode().expect("get mode"); // TODO: properly propagate this error
                    let file_size = entry.header().size().ok();
                    let mut buffer = Vec::with_capacity(entry.size() as usize);
                    entry.read_to_end(&mut buffer).expect("read content"); // TODO: properly propagate this error
                    FileInfo { cleaned_entry_path, file_mode, file_size, buffer }
                })
                .collect::<Vec<FileInfo>>()
                .into_par_iter()
                .map(|file_info| -> Result<_, TarballError> {
                    let FileInfo { cleaned_entry_path, file_mode, file_size, buffer } = file_info;
                    let file_is_executable = file_mode::is_all_exec(file_mode);

                    let (file_path, file_hash) = store_dir
                        .write_cas_file(&buffer, file_is_executable)
                        .map_err(TarballError::WriteCasFile)?;

                    let index_key = cleaned_entry_path
                        .to_str()
                        .expect("entry path must be valid UTF-8") // TODO: propagate this error, provide more information
                        .to_string(); // TODO: convert cleaned_entry_path to String too.

                    let checked_at = UNIX_EPOCH.elapsed().ok().map(|x| x.as_millis());
                    let file_integrity = format!("sha512-{}", BASE64_STD.encode(file_hash));
                    let index_value = PackageFileInfo {
                        checked_at,
                        integrity: file_integrity,
                        mode: file_mode,
                        size: file_size,
                    };

                    Ok(((cleaned_entry_path, file_path), (index_key, index_value)))
                })
                .collect::<Result<(HashMap<_, _>, HashMap<_, _>), TarballError>>()?;

            let pkg_files_idx = PackageFilesIndex { files: index_entries };

            store_dir
                .write_index_file(&package_integrity, &pkg_files_idx)
                .map_err(TarballError::WriteTarballIndexFile)?;

            Ok::<_, TarballError>(cas_paths)
        });

        verify_checksum_task.await.expect("no join error").map_err(|error| {
            TarballError::Checksum(VerifyChecksumError { url: package_url.to_string(), error })
        })?;

        tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

        let cas_paths = extract_tarball_task.await.expect("no join error")?;

        tracing::info!(target: "pacquet::download", ?package_url, "Tarball extracted");

        Ok(cas_paths)
    }
}

#[cfg(test)]
mod tests {
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use tempfile::{tempdir, TempDir};

    use super::*;

    fn integrity(integrity_str: &str) -> Integrity {
        integrity_str.parse().expect("parse integrity string")
    }

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
    fn tempdir_with_leaked_path() -> (TempDir, &'static StoreDir) {
        let tempdir = tempdir().unwrap();
        let leaked_path =
            tempdir.path().to_path_buf().pipe(StoreDir::from).pipe(Box::new).pipe(Box::leak);
        (tempdir, leaked_path)
    }

    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn packages_under_orgs_should_work() {
        let (store_dir, store_path) = tempdir_with_leaked_path();
        let cas_files = DownloadTarballToStore {
            http_client: &Default::default(),
            store_dir: store_path,
            package_integrity: &integrity("sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz"
        }
        .without_cache()
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
            http_client: &Default::default(),
            store_dir: store_path,
            package_integrity: &integrity("sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        }
        .without_cache()
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }
}
