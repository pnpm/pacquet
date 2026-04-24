use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::PathBuf,
    sync::Arc,
    time::UNIX_EPOCH,
};

use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_fs::file_mode;
use pacquet_network::ThrottledClient;
use pacquet_store_dir::{
    store_index_key, CafsFileInfo, PackageFilesIndex, SharedReadonlyStoreIndex, StoreDir,
    StoreIndex, StoreIndexError, WriteCasFileError,
};
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
    #[display("Failed to decode gzip: {_0}")]
    #[diagnostic(code(pacquet_tarball::decode_gzip))]
    DecodeGzip(InflateDecodeErrors),

    #[from(ignore)]
    #[display("Failed to write cafs: {_0}")]
    #[diagnostic(transparent)]
    WriteCasFile(WriteCasFileError),

    #[from(ignore)]
    #[display("Failed to write store index (SQLite index): {_0}")]
    #[diagnostic(transparent)]
    WriteStoreIndex(StoreIndexError),

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
    Available(Arc<HashMap<String, PathBuf>>),
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

/// Try to reconstruct the `{filename → CAFS path}` map for a package from
/// the SQLite store index, without going to the network. Returns `None`
/// if anything looks off — no index handed in, no row, unreadable row,
/// malformed digest, or any referenced CAFS path that fails validation —
/// so the caller falls through to a fresh download. Error paths are
/// treated as cache misses because the index is a cache hint, not a
/// source of truth. Any CAFS-path validation failure (missing blob,
/// metadata error, directory, symlink, or other non-regular file) emits
/// a `debug!` log to note the stale entry before re-fetching; earlier
/// checks — row / decode / digest — are silent because they don't point
/// at a specific on-disk artifact worth describing.
///
/// The `index` argument is a shared read-only handle that callers open
/// once per install and pass in repeatedly, so we don't pay the
/// `Connection::open` + PRAGMA cost per package.
async fn load_cached_cas_paths(
    index: Option<SharedReadonlyStoreIndex>,
    store_dir: &'static StoreDir,
    cache_key: String,
) -> Option<HashMap<String, PathBuf>> {
    let index = index?;
    // Hold on to a copy of the cache key for the outer `JoinError` log,
    // since the task body moves the original in.
    let outer_cache_key = cache_key.clone();
    let result = tokio::task::spawn_blocking(move || -> Option<HashMap<String, PathBuf>> {
        // Treat a poisoned mutex as a cache miss rather than propagating the
        // panic: the `SELECT` is stateless, so the prior panic couldn't have
        // left the index in an inconsistent shape, and cache lookups are a
        // best-effort hint anyway — failing over to a fresh download is the
        // more resilient default than turning every subsequent snapshot into
        // a crash.
        let entry = {
            let Ok(guard) = index.lock() else {
                tracing::debug!(
                    target: "pacquet::download",
                    ?cache_key,
                    "store-index mutex poisoned; treating cache lookup as a miss",
                );
                return None;
            };
            guard.get(&cache_key).ok()?
        }?;

        let mut cas_paths = HashMap::with_capacity(entry.files.len());
        // Consume `entry.files` so the owned `String` keys can move
        // straight into `cas_paths` — cloning each filename is an extra
        // alloc per file in the package, and on a real tarball that's
        // hundreds of strings.
        for (filename, info) in entry.files {
            // `?` on `cas_file_path_by_mode` handles corrupt digests (empty,
            // too short, or non-hex) as a cache miss. Without it the
            // `hex[..2]` slice inside `file_path_by_hex_str` would panic.
            let path = store_dir.cas_file_path_by_mode(&info.digest, info.mode)?;
            // Use `symlink_metadata()` + reject symlinks so this check
            // applies to the CAFS path itself without ever following a
            // link. That rules out directory squatting, symlinked
            // blobs (which could point *outside* the store — a store
            // corruption / tampering vector), and other non-regular
            // filesystem objects. Any metadata error also counts as a
            // miss (blob pruned, permission issue, …).
            if !path.symlink_metadata().is_ok_and(|m| {
                let file_type = m.file_type();
                !file_type.is_symlink() && file_type.is_file()
            }) {
                // Treat the whole entry as invalid and re-fetch — partial
                // reuse would give the caller a broken layout.
                tracing::debug!(
                    target: "pacquet::download",
                    ?cache_key,
                    ?filename,
                    ?path,
                    "CAFS path missing or not a regular file; index entry is stale, re-fetching"
                );
                return None;
            }
            cas_paths.insert(filename, path);
        }
        Some(cas_paths)
    })
    .await;

    match result {
        Ok(cas_paths) => cas_paths,
        Err(error) => {
            // `JoinError` — the blocking task panicked, or the runtime was
            // cancelled mid-install. Degrade to a cache miss so the caller
            // falls through to a fresh download, but surface the error so
            // the panic / cancellation stays diagnosable.
            tracing::warn!(
                target: "pacquet::download",
                ?error,
                cache_key = ?outer_cache_key,
                "store-index lookup task failed; treating cache lookup as a miss",
            );
            None
        }
    }
}

/// This subroutine downloads and extracts a tarball to the store directory.
///
/// It returns a CAS map of files in the tarball.
#[must_use]
pub struct DownloadTarballToStore<'a> {
    pub http_client: &'a ThrottledClient,
    pub store_dir: &'static StoreDir,
    /// Shared read-only handle to the SQLite store index. `None` when the
    /// store does not (yet) have an `index.db`, in which case every cache
    /// lookup short-circuits to a network fetch. Callers open this once per
    /// install and pass the same handle to every `DownloadTarballToStore`
    /// so we don't reopen the DB per package.
    pub store_index: Option<SharedReadonlyStoreIndex>,
    pub package_integrity: &'a Integrity,
    pub package_unpacked_size: Option<usize>,
    pub package_url: &'a str,
    /// Stable identifier for the package, e.g. `"{name}@{version}"`. Paired
    /// with `package_integrity` to form the SQLite index key per pnpm v11's
    /// `storeIndexKey`.
    pub package_id: &'a str,
}

impl<'a> DownloadTarballToStore<'a> {
    /// Execute the subroutine with an in-memory cache.
    pub async fn run_with_mem_cache(
        self,
        mem_cache: &'a MemCache,
    ) -> Result<Arc<HashMap<String, PathBuf>>, TarballError> {
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
    pub async fn run_without_mem_cache(&self) -> Result<HashMap<String, PathBuf>, TarballError> {
        let &DownloadTarballToStore {
            http_client,
            store_dir,
            package_integrity,
            package_unpacked_size,
            package_url,
            package_id,
            ..
        } = self;
        let store_index = self.store_index.clone();

        // Before hitting the network, check the SQLite store index: if the
        // tarball is already in the CAFS we can reuse its per-file paths
        // and skip the download entirely. This is the payoff of the v11
        // store migration (#244) — pnpm and pacquet share `index.db`, so a
        // previous install of the same (integrity, pkg_id) pair leaves an
        // entry we can read back here.
        //
        // The lookup is best-effort. A missing `index.db`, a missing row,
        // an undecodable entry, or any CAFS file that has gone missing
        // from disk all fall through to the download path below.
        let cache_key = store_index_key(&package_integrity.to_string(), package_id);
        if let Some(cas_paths) = load_cached_cas_paths(store_index, store_dir, cache_key).await {
            tracing::info!(target: "pacquet::download", ?package_url, ?package_id, "Reusing cached CAFS entry — skipping download");
            return Ok(cas_paths);
        }

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
        let package_id = package_id.to_string();

        #[derive(Debug, From)]
        enum TaskError {
            Checksum(ssri::Error),
            Other(TarballError),
        }
        // Run the whole post-download pipeline on the blocking pool. The
        // body is a dense mix of CPU-bound work (SHA-512 over the full
        // tarball, gzip inflate, per-file SHA-512) and blocking I/O
        // (`write_cas_file`, SQLite open + INSERT). Running it as a plain
        // `tokio::task::spawn` pinned a tokio reactor worker for the
        // entirety of each tarball — on a 2-core runner that meant at
        // most two tarballs could make progress at a time, and the tail
        // of large packages missed the CI step budget even though the
        // network side was long done (#268 — all downloads complete,
        // ~1115 tarballs stuck between "Download completed" and
        // "Checksum verified"). `spawn_blocking` uses tokio's dedicated
        // blocking-thread pool (default 512) instead, so the tail drains
        // in parallel.
        let cas_paths = tokio::task::spawn_blocking(
            move || -> Result<HashMap<String, PathBuf>, TaskError> {
                package_integrity.check(&response).map_err(TaskError::Checksum)?;

                // TODO: move tarball extraction to its own function
                // TODO: test it
                // TODO: test the duplication of entries

                // Extract the tarball in a scope so the decompressed
                // buffer + `tar::Archive` are released before the SQLite
                // write — on large packages the inflated bytes can be
                // multiple MB, and with hundreds of concurrent blocking
                // tasks that memory adds up fast.
                let (cas_paths, pkg_files_idx) = {
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
                    let mut cas_paths = HashMap::<String, PathBuf>::with_capacity(capacity);
                    let mut pkg_files_idx = PackageFilesIndex {
                        manifest: None,
                        requires_build: None,
                        algo: "sha512".to_string(),
                        files: HashMap::with_capacity(capacity),
                        side_effects: None,
                    };

                    for entry in entries {
                        let mut entry = entry.unwrap();

                        let file_mode = entry.header().mode().expect("get mode"); // TODO: properly propagate this error
                        let file_is_executable = file_mode::is_executable(file_mode);

                        // Read the contents of the entry
                        let mut buffer = Vec::with_capacity(entry.size() as usize);
                        entry.read_to_end(&mut buffer).unwrap();

                        let entry_path = entry.path().unwrap();
                        let cleaned_entry_path = entry_path
                            .components()
                            .skip(1)
                            .collect::<PathBuf>()
                            .into_os_string()
                            .into_string()
                            .expect("entry path must be valid UTF-8");
                        let (file_path, file_hash) = store_dir
                            .write_cas_file(&buffer, file_is_executable)
                            .map_err(TarballError::WriteCasFile)?;

                        if let Some(previous) =
                            cas_paths.insert(cleaned_entry_path.clone(), file_path)
                        {
                            tracing::warn!(
                                ?previous,
                                "Duplication detected. Old entry has been ejected"
                            );
                        }

                        // `as_millis()` returns `u128`; narrow to `u64` to match
                        // the store index schema — see `CafsFileInfo::checked_at`
                        // for why `u64` is used. Using `u64::try_from` rather
                        // than `as u64` avoids a silent wrap: even though
                        // millisecond epochs don't overflow `u64` for ~584M
                        // years, the intent should be explicit. If the clock
                        // ever reports something unrepresentable, drop the
                        // timestamp — the `checkedAt` field is optional and
                        // pnpm tolerates `None`.
                        let checked_at = UNIX_EPOCH
                            .elapsed()
                            .ok()
                            .and_then(|x| u64::try_from(x.as_millis()).ok());
                        let file_size = entry
                            .header()
                            .size()
                            .map_err(TarballError::ReadTarballEntries)
                            .map_err(TaskError::Other)?;
                        let file_attrs = CafsFileInfo {
                            digest: format!("{file_hash:x}"),
                            mode: file_mode,
                            size: file_size,
                            checked_at,
                        };

                        if let Some(previous) =
                            pkg_files_idx.files.insert(cleaned_entry_path, file_attrs)
                        {
                            tracing::warn!(
                                ?previous,
                                "Duplication detected. Old entry has been ejected"
                            );
                        }
                    }

                    (cas_paths, pkg_files_idx)
                };

                // Record the per-tarball file index in the shared SQLite
                // index so other pacquet / pnpm processes can find these
                // files on disk. We're already on the blocking pool, so
                // the synchronous `Connection::open` + PRAGMA + INSERT
                // run inline — no nested `spawn_blocking` needed.
                // SQLite serializes concurrent writers via its
                // `busy_timeout=5000 ms`.
                let index_key = store_index_key(&package_integrity.to_string(), &package_id);
                let v11_dir = store_dir.v11();
                StoreIndex::open(&v11_dir)
                    .and_then(|index| index.set(&index_key, &pkg_files_idx))
                    .map_err(TarballError::WriteStoreIndex)?;

                Ok(cas_paths)
            },
        )
        .await
        .expect("tarball-processing blocking task panicked")
        .map_err(|error| match error {
            TaskError::Checksum(error) => {
                TarballError::Checksum(VerifyChecksumError { url: package_url.to_string(), error })
            }
            TaskError::Other(error) => error,
        })?;

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

    /// HTTP client for the fall-through tests. A default `ThrottledClient`
    /// uses `Client::new()` with no connect / request timeout, so on a
    /// firewalled runner the unreachable `http://127.0.0.1:1/...` URL
    /// could stall for minutes of TCP retry. One-second bounds are
    /// plenty for loopback and keep the failure mode deterministic.
    fn fast_fail_client() -> ThrottledClient {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(1))
            .timeout(std::time::Duration::from_secs(1))
            .build()
            .expect("build reqwest client");
        ThrottledClient::from_client(client)
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
            store_index: None,
            package_integrity: &integrity("sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            package_id: "@fastify/error@3.3.0",
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
            store_index: None,
            package_integrity: &integrity("sha512-aaaan1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w=="),
            package_unpacked_size: Some(16697),
            package_url: "https://registry.npmjs.org/@fastify/error/-/error-3.3.0.tgz",
            package_id: "@fastify/error@3.3.0",
        }
        .run_without_mem_cache()
        .await
        .expect_err("checksum mismatch");

        drop(store_dir);
    }

    /// When the SQLite index already has an entry for this
    /// `(integrity, pkg_id)` pair and every referenced CAFS file is on
    /// disk, `run_without_mem_cache` must return the cached layout
    /// without issuing an HTTP request. We prove the "no network"
    /// property by pointing `package_url` at an address that would
    /// fail-fast if dialed.
    #[tokio::test]
    async fn reuses_cached_cas_paths_when_index_entry_is_live() {
        let (store_dir, store_path) = tempdir_with_leaked_path();

        let (pkg_json_path, pkg_json_hash) =
            store_path.write_cas_file(b"{\"name\":\"fake\"}", false).unwrap();
        let (bin_path, bin_hash) =
            store_path.write_cas_file(b"#!/usr/bin/env node\nconsole.log('hi');\n", true).unwrap();

        let pkg_integrity =
            integrity("sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==");
        let pkg_id = "fake@1.0.0";
        let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            CafsFileInfo {
                digest: format!("{pkg_json_hash:x}"),
                mode: 0o644,
                size: 15,
                checked_at: None,
            },
        );
        files.insert(
            "bin/cli.js".to_string(),
            CafsFileInfo {
                digest: format!("{bin_hash:x}"),
                mode: 0o755,
                size: 39,
                checked_at: None,
            },
        );

        let entry = PackageFilesIndex {
            manifest: None,
            requires_build: Some(false),
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };

        let index = StoreIndex::open_in(store_path).unwrap();
        index.set(&index_key, &entry).unwrap();
        drop(index);

        let cas_paths = DownloadTarballToStore {
            http_client: &fast_fail_client(),
            store_dir: store_path,
            store_index: StoreIndex::shared_readonly_in(store_path),
            package_integrity: &pkg_integrity,
            package_unpacked_size: None,
            // Any request that reaches the network here would fail the
            // test; the cache lookup must short-circuit before we get
            // near it. `fast_fail_client` caps that at 1 s per side in
            // case a firewalled runner drops the packet silently.
            package_url: "http://127.0.0.1:1/unreachable.tgz",
            package_id: pkg_id,
        }
        .run_without_mem_cache()
        .await
        .expect("cache hit should succeed without network");

        assert_eq!(cas_paths.len(), 2);
        assert_eq!(cas_paths.get("package.json"), Some(&pkg_json_path));
        assert_eq!(cas_paths.get("bin/cli.js"), Some(&bin_path));

        drop(store_dir);
    }

    /// If the index row points at a CAFS blob that no longer exists on
    /// disk (pruned out-of-band, say), the cache lookup must reject the
    /// entry and fall through to a download. We don't want to do the
    /// download for real in a unit test, so assert that we got a
    /// `FetchTarball` error from the unreachable URL rather than the
    /// cache-hit's `Ok`.
    #[tokio::test]
    async fn falls_through_when_cafs_file_missing() {
        let (store_dir, store_path) = tempdir_with_leaked_path();

        let pkg_integrity =
            integrity("sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==");
        let pkg_id = "fake@1.0.0";
        let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

        let mut files = HashMap::new();
        // A digest that matches no file on disk. `load_cached_cas_paths`
        // should see the missing path, reject the entry, and let
        // `run_without_mem_cache` proceed to the network fetch.
        files.insert(
            "package.json".to_string(),
            CafsFileInfo { digest: "0".repeat(128), mode: 0o644, size: 0, checked_at: None },
        );

        let entry = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let index = StoreIndex::open_in(store_path).unwrap();
        index.set(&index_key, &entry).unwrap();
        drop(index);

        let err = DownloadTarballToStore {
            http_client: &fast_fail_client(),
            store_dir: store_path,
            store_index: StoreIndex::shared_readonly_in(store_path),
            package_integrity: &pkg_integrity,
            package_unpacked_size: None,
            package_url: "http://127.0.0.1:1/unreachable.tgz",
            package_id: pkg_id,
        }
        .run_without_mem_cache()
        .await
        .expect_err("stale index entry must not resolve to a cache hit");
        assert!(
            matches!(err, TarballError::FetchTarball(_)),
            "expected fall-through to network fetch, got: {err:?}"
        );

        drop(store_dir);
    }

    /// A corrupt row whose digest is empty (or too short / non-hex) used
    /// to panic inside `StoreDir::file_path_by_hex_str` (`hex[..2]`). The
    /// validation in `cas_file_path_by_mode` now rejects such rows, and
    /// `load_cached_cas_paths` treats that as a cache miss.
    #[tokio::test]
    async fn falls_through_when_digest_is_malformed() {
        let (store_dir, store_path) = tempdir_with_leaked_path();

        let pkg_integrity =
            integrity("sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==");
        let pkg_id = "fake@1.0.0";
        let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            // Empty digest — pre-fix this would panic in the spawn_blocking
            // task during `hex[..2]`.
            CafsFileInfo { digest: String::new(), mode: 0o644, size: 0, checked_at: None },
        );
        let entry = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let index = StoreIndex::open_in(store_path).unwrap();
        index.set(&index_key, &entry).unwrap();
        drop(index);

        let err = DownloadTarballToStore {
            http_client: &fast_fail_client(),
            store_dir: store_path,
            store_index: StoreIndex::shared_readonly_in(store_path),
            package_integrity: &pkg_integrity,
            package_unpacked_size: None,
            package_url: "http://127.0.0.1:1/unreachable.tgz",
            package_id: pkg_id,
        }
        .run_without_mem_cache()
        .await
        .expect_err("corrupt digest must not resolve to a cache hit");
        assert!(
            matches!(err, TarballError::FetchTarball(_)),
            "expected fall-through to network fetch, got: {err:?}"
        );

        drop(store_dir);
    }

    /// A corrupted store might have a directory sitting where a CAFS blob
    /// belongs (stray `mkdir -p`, interrupted write, whatever). `exists()`
    /// would have let it through; `metadata().is_file()` rejects it.
    #[tokio::test]
    async fn falls_through_when_cafs_path_is_a_directory() {
        let (store_dir, store_path) = tempdir_with_leaked_path();

        let pkg_integrity =
            integrity("sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==");
        let pkg_id = "fake@1.0.0";
        let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

        let digest = "a".repeat(128);
        let cafs_path = store_path
            .cas_file_path_by_mode(&digest, 0o644)
            .expect("128-char hex must produce a valid CAFS path");
        std::fs::create_dir_all(&cafs_path).unwrap();

        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            CafsFileInfo { digest, mode: 0o644, size: 0, checked_at: None },
        );
        let entry = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let index = StoreIndex::open_in(store_path).unwrap();
        index.set(&index_key, &entry).unwrap();
        drop(index);

        let err = DownloadTarballToStore {
            http_client: &fast_fail_client(),
            store_dir: store_path,
            store_index: StoreIndex::shared_readonly_in(store_path),
            package_integrity: &pkg_integrity,
            package_unpacked_size: None,
            package_url: "http://127.0.0.1:1/unreachable.tgz",
            package_id: pkg_id,
        }
        .run_without_mem_cache()
        .await
        .expect_err("directory at CAFS path must not resolve to a cache hit");
        assert!(
            matches!(err, TarballError::FetchTarball(_)),
            "expected fall-through to network fetch, got: {err:?}"
        );

        drop(store_dir);
    }

    /// A symlink at the CAFS path — even one pointing at a valid regular
    /// file — must not be trusted. A tampered / corrupted store could
    /// place one pointing outside the store entirely, so we use
    /// `symlink_metadata()` and reject symlinks regardless of target.
    #[tokio::test]
    #[cfg(not(target_os = "windows"))]
    async fn falls_through_when_cafs_path_is_a_symlink() {
        let (store_dir, store_path) = tempdir_with_leaked_path();

        let pkg_integrity =
            integrity("sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==");
        let pkg_id = "fake@1.0.0";
        let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

        let digest = "b".repeat(128);
        let cafs_path = store_path
            .cas_file_path_by_mode(&digest, 0o644)
            .expect("128-char hex must produce a valid CAFS path");
        std::fs::create_dir_all(cafs_path.parent().unwrap()).unwrap();

        // Plant a symlink at the CAFS path pointing at a real regular
        // file elsewhere. `metadata()` would have followed it and the
        // check would have (incorrectly) succeeded; `symlink_metadata()`
        // must reject the link itself.
        let target = store_dir.path().join("outside-the-cafs.txt");
        std::fs::write(&target, b"evil").unwrap();
        std::os::unix::fs::symlink(&target, &cafs_path).unwrap();

        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            CafsFileInfo { digest, mode: 0o644, size: 4, checked_at: None },
        );
        let entry = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let index = StoreIndex::open_in(store_path).unwrap();
        index.set(&index_key, &entry).unwrap();
        drop(index);

        let err = DownloadTarballToStore {
            http_client: &fast_fail_client(),
            store_dir: store_path,
            store_index: StoreIndex::shared_readonly_in(store_path),
            package_integrity: &pkg_integrity,
            package_unpacked_size: None,
            package_url: "http://127.0.0.1:1/unreachable.tgz",
            package_id: pkg_id,
        }
        .run_without_mem_cache()
        .await
        .expect_err("symlink at CAFS path must not resolve to a cache hit");
        assert!(
            matches!(err, TarballError::FetchTarball(_)),
            "expected fall-through to network fetch, got: {err:?}"
        );

        drop(store_dir);
    }
}
