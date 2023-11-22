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
use pacquet_fs::{file_mode, IoSendError, IoTaskError, IoThread};
use pacquet_network::ThrottledClient;
use pacquet_store_dir::{PackageFileInfo, PackageFilesIndex, StoreDir, WriteIndexFileError};
use pipe_trait::Pipe;
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
    #[display("Failed to send command to write cafs: {_0}")]
    SendWriteCasFile(IoSendError),

    #[from(ignore)]
    #[display("Failed to write cafs: {_0}")]
    #[diagnostic(transparent)]
    WriteCasFile(IoTaskError),

    #[from(ignore)]
    #[display("Failed to write tarball index: {_0}")]
    #[diagnostic(transparent)]
    WriteTarballIndexFile(WriteIndexFileError),

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

/// Internal in-memory cache of tarballs.
///
/// The key of this hashmap is the url of each tarball.
pub type MemCache = DashMap<String, Arc<RwLock<CacheValue>>>;

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

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub io_thread: &'a IoThread,
    pub store_dir: &'static StoreDir,
    pub package_integrity: &'a Integrity,
    pub package_unpacked_size: Option<usize>,
    pub package_url: &'a str,
}

impl<'a> DownloadTarballToStore<'a> {
    /// Execute the subroutine with an in-memory cache.
    pub async fn run_with_mem_cache(
        self,
        mem_cache: &'a MemCache,
    ) -> Result<Arc<HashMap<OsString, PathBuf>>, TarballError> {
        let &DownloadTarballToStore { package_url, .. } = &self;

        // QUESTION: I see no copying from existing store_dir, is there such mechanism?
        // TODO: If it's not implemented yet, implement it

        if let Some(cache_lock) = mem_cache.get(package_url) {
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
            if mem_cache.insert(package_url.to_string(), Arc::clone(&cache_lock)).is_some() {
                tracing::warn!(target: "pacquet::download", ?package_url, "Race condition detected when writing to cache");
            }
            let cas_paths = self.run_without_mem_cache().await?.pipe(Arc::new);
            let mut cache_write = cache_lock.write().await;
            *cache_write = CacheValue::Available(Arc::clone(&cas_paths));
            notify.notify_waiters();
            Ok(cas_paths)
        }
    }

    /// Execute the subroutine without an in-memory cache.
    pub async fn run_without_mem_cache(&self) -> Result<HashMap<OsString, PathBuf>, TarballError> {
        let &DownloadTarballToStore {
            http_client,
            io_thread,
            store_dir,
            package_integrity,
            package_unpacked_size,
            package_url,
        } = self;

        tracing::info!(target: "pacquet::download", ?package_url, "New cache");

        let network_error = |error| {
            TarballError::FetchTarball(NetworkError { url: package_url.to_string(), error })
        };
        let response = http_client
            .run_with_permit(|client| client.get(package_url).send())
            .await
            .map_err(network_error)?
            .bytes()
            .await
            .map_err(network_error)?;

        tracing::info!(target: "pacquet::download", ?package_url, "Download completed");

        // TODO: Cloning here is less than desirable, there are 2 possible solutions for this problem:
        // 1. Use an Arc and convert this line to Arc::clone.
        // 2. Replace ssri with base64 and serde magic (which supports Copy).
        let package_integrity = package_integrity.clone();

        package_integrity.check(&response).map_err(TarballError::Integrity)?;

        // TODO: move tarball extraction to its own function
        // TODO: test it
        // TODO: test the duplication of entries

        let mut archive =
            decompress_gzip(&response, package_unpacked_size)?.pipe(Cursor::new).pipe(Archive::new);

        let entries = archive
            .entries()
            .map_err(TarballError::ReadTarballEntries)?
            .filter(|entry| !entry.as_ref().unwrap().header().entry_type().is_dir());

        let ((_, Some(capacity)) | (capacity, None)) = entries.size_hint();
        let mut cas_paths = HashMap::<OsString, PathBuf>::with_capacity(capacity);
        let mut pkg_files_idx = PackageFilesIndex { files: HashMap::with_capacity(capacity) };

        for entry in entries {
            let mut entry = entry.unwrap();

            let file_mode = entry.header().mode().expect("get mode"); // TODO: properly propagate this error
            let file_is_executable = file_mode::is_all_exec(file_mode);

            // Read the contents of the entry
            let mut buffer = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buffer).unwrap();

            let entry_path = entry.path().unwrap();
            let cleaned_entry_path =
                entry_path.components().skip(1).collect::<PathBuf>().into_os_string();
            let (file_path, file_hash, _receiver) = store_dir
                .write_cas_file_thread(io_thread, buffer, file_is_executable)
                .map_err(TarballError::SendWriteCasFile)?;

            // // TODO: should this be defer to the end?
            // receiver
            //     .await
            //     .expect("the channel shouldn't be dropped this soon")
            //     .map_err(TarballError::WriteCasFile)?;

            let tarball_index_key = cleaned_entry_path
                .to_str()
                .expect("entry path must be valid UTF-8") // TODO: propagate this error, provide more information
                .to_string(); // TODO: convert cleaned_entry_path to String too.

            if let Some(previous) = cas_paths.insert(cleaned_entry_path, file_path) {
                tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
            }

            let checked_at = UNIX_EPOCH.elapsed().ok().map(|x| x.as_millis());
            let file_size = entry.header().size().ok();
            let file_integrity = format!("sha512-{}", BASE64_STD.encode(file_hash));
            let file_attrs = PackageFileInfo {
                checked_at,
                integrity: file_integrity,
                mode: file_mode,
                size: file_size,
            };

            if let Some(previous) = pkg_files_idx.files.insert(tarball_index_key, file_attrs) {
                tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
            }
        }

        store_dir
            .write_index_file(&package_integrity, &pkg_files_idx)
            .map_err(TarballError::WriteTarballIndexFile)?;

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
            io_thread: &IoThread::spawn(),
            store_dir: store_path,
            package_integrity: &integrity("sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz"
        }
        .run_without_mem_cache()
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
            io_thread: &IoThread::spawn(),
            package_integrity: &integrity("sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
        }
        .run_without_mem_cache()
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }
}
