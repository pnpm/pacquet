use std::{
    collections::HashMap,
    ffi::OsString,
    io::{Cursor, Read},
    path::PathBuf,
    sync::Arc,
};

use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine};
use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_store_dir::{
    FileSuffix, StoreDir, TarballIndex, TarballIndexFileAttrs, WriteNonIndexFileError,
    WriteTarballIndexFileError,
};
use pipe_trait::Pipe;
use reqwest::Client;
use ssri::{Integrity, IntegrityChecker};
use tar::Archive;
use tokio::sync::{Notify, RwLock};
use tracing::instrument;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

/// Mask of the executable bits.
///
/// This value is equal to `S_IXUSR` in libc.
const EXEC_MASK: u32 = 64;

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to fetch {url}: {error}")]
pub struct NetworkError {
    pub url: String,
    pub error: reqwest::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Cannot parse {integrity:?} from {url} as an integrity: {error}")]
pub struct ParseIntegrityError {
    pub url: String,
    pub integrity: String,
    #[error(source)]
    pub error: ssri::Error,
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

    #[diagnostic(code(pacquet_tarball::parse_integrity_error))]
    ParseIntegrity(ParseIntegrityError),

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
    WriteNonIndexFile(WriteNonIndexFileError),

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
fn verify_checksum(data: &[u8], integrity: Integrity) -> Result<ssri::Algorithm, ssri::Error> {
    integrity.pipe(IntegrityChecker::new).chain(data).result()
}

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    pub tarball_cache: &'a Cache,
    pub http_client: &'a Client,
    pub store_dir: &'static StoreDir,
    pub package_integrity: &'a str,
    pub package_unpacked_size: Option<usize>,
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
        #[derive(Debug, From)]
        enum TaskError {
            Checksum(ssri::Error),
            Other(TarballError),
        }
        let cas_paths = tokio::task::spawn(async move {
            verify_checksum(&response, package_integrity.clone()).map_err(TaskError::Checksum)?;

            let mut archive = decompress_gzip(&response, package_unpacked_size)
                .map_err(TaskError::Other)?
                .pipe(Cursor::new)
                .pipe(Archive::new);

            let entries = archive
                .entries()
                .map_err(TarballError::ReadTarballEntries)
                .map_err(TaskError::Other)?
                .filter(|entry| !entry.as_ref().unwrap().header().entry_type().is_dir());

            let ((_, Some(capacity)) | (capacity, None)) = entries.size_hint();
            let mut cas_paths = HashMap::<OsString, PathBuf>::with_capacity(capacity);
            let mut tarball_index = TarballIndex { files: HashMap::with_capacity(capacity) };

            for entry in entries {
                let mut entry = entry.unwrap();

                let file_mode = entry.header().mode().expect("get mode"); // TODO: properly propagate this error
                let is_executable = file_mode & EXEC_MASK != 0;
                let file_suffix = is_executable.then_some(FileSuffix::Exec);

                // Read the contents of the entry
                let mut buffer = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buffer).unwrap();

                let entry_path = entry.path().unwrap();
                let cleaned_entry_path =
                    entry_path.components().skip(1).collect::<PathBuf>().into_os_string();
                let (file_path, file_hash) = store_dir
                    .write_non_index_file(&buffer, file_suffix)
                    .map_err(TarballError::WriteNonIndexFile)?;

                let tarball_index_key = cleaned_entry_path
                    .to_str()
                    .expect("entry path must be valid UTF-8") // TODO: propagate this error, provide more information
                    .to_string(); // TODO: convert cleaned_entry_path to String too.

                if let Some(previous) = cas_paths.insert(cleaned_entry_path, file_path) {
                    panic!("Unexpected error: {previous:?} shouldn't collide");
                }

                let file_size = entry.header().size().ok();
                let file_integrity = format!("sha512-{}", BASE64_STD.encode(file_hash));
                let file_attrs = TarballIndexFileAttrs {
                    integrity: file_integrity,
                    mode: file_mode,
                    size: file_size,
                };

                if let Some(previous) = tarball_index.files.insert(tarball_index_key, file_attrs) {
                    panic!("Unexpected error: {previous:?} shouldn't collide");
                }
            }

            store_dir
                .write_tarball_index_file(&package_integrity, &tarball_index)
                .map_err(TarballError::WriteTarballIndexFile)?;

            Ok(cas_paths)
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
