use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Component, PathBuf},
    sync::{Arc, OnceLock},
    time::UNIX_EPOCH,
};

use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_fs::file_mode;
use pacquet_network::ThrottledClient;
use pacquet_store_dir::{
    store_index_key, CafsFileInfo, PackageFilesIndex, SharedReadonlyStoreIndex, StoreDir,
    StoreIndexError, StoreIndexWriter, WriteCasFileError,
};
use pipe_trait::Pipe;
use ssri::Integrity;
use tar::Archive;
use tokio::sync::{Notify, RwLock, Semaphore};
use tracing::instrument;
use zune_inflate::{errors::InflateDecodeErrors, DeflateDecoder, DeflateOptions};

/// Cap on concurrent post-download tarball work (SHA-512 of the whole
/// tarball + gzip inflate + per-file SHA-512 + CAFS writes). The body is
/// CPU-bound with some blocking FS I/O, and putting it on
/// `tokio::task::spawn_blocking` makes the default 512-thread blocking
/// pool available — but async fan-out across `try_join_all` routinely
/// fires hundreds of these at once on a 1352-snapshot install, which
/// thrashes small CI runners. Past "Download completed" a 2-CPU GitHub
/// Actions runner wedged between decompress-close and `Checksum verified`
/// on #269 until the step timeout. `num_cpus * 2` (floor 4) keeps enough
/// work in flight to overlap per-file FS writes with SHA on another task
/// without oversubscribing the cores.
fn post_download_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(num_cpus::get().saturating_mul(2).max(4)))
}

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

    #[from(ignore)]
    #[display(
        "Tarball at {url} advertised a Content-Length of {advertised_size} bytes, which exceeds what pacquet can allocate (either larger than `usize::MAX` on this target or memory pressure prevented a one-shot reservation)"
    )]
    #[diagnostic(code(pacquet_tarball::tarball_too_large))]
    TarballTooLarge { url: String, advertised_size: u64 },
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

/// Build the buffer that the tarball body streams into, pre-sized
/// from the response's advertised `Content-Length` when it fits and
/// can actually be reserved without allocation failure.
///
/// `Content-Length` is untrusted input — a malicious or broken
/// registry could advertise `u64::MAX`, which would crash the
/// process if we passed it directly to `Vec::with_capacity`. Two
/// guards:
///
/// 1. `usize::try_from(size)` — on 32-bit targets a `u64` header
///    value may exceed `usize::MAX`; on 64-bit the two are the
///    same width but the conversion is cheap anyway.
/// 2. `Vec::try_reserve_exact(cap)` — if the allocator refuses
///    (legitimate OOM, or because `cap` is absurdly large relative
///    to available RAM), we surface `TarballTooLarge` instead of
///    aborting via the infallible `with_capacity` path.
///
/// When `content_length` is absent the response uses chunked
/// transfer encoding and we can't pre-size; return an empty
/// growable `Vec` and let the stream loop extend it.
fn allocate_tarball_buffer(
    content_length: Option<u64>,
    url: &str,
) -> Result<Vec<u8>, TarballError> {
    let Some(size) = content_length else {
        return Ok(Vec::new());
    };

    let too_large =
        || TarballError::TarballTooLarge { url: url.to_string(), advertised_size: size };

    let capacity = usize::try_from(size).map_err(|_| too_large())?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(capacity).map_err(|_| too_large())?;
    Ok(buf)
}

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

/// Walk a decompressed tar archive, writing each regular-file entry
/// into the CAFS and returning the `{in-tarball path → CAFS path}` map
/// plus the per-tarball [`PackageFilesIndex`] row to hand off to the
/// shared store-index writer.
///
/// Non-regular-file entries (symlinks, hardlinks, character / block
/// devices, fifos, GNU / PAX extension headers, directories) are
/// filtered out. Real npm-publish tarballs only carry regular files;
/// anything else would need custom handling that pacquet doesn't yet
/// do, and silently reading a symlink's 0-byte body into the CAFS as
/// if it were a file would just corrupt the store.
///
/// Every tar-side failure — a corrupt entries iterator, a mangled
/// header (bad mode, bad size), a short body read, a path decode error,
/// a path whose components would escape the CAFS root — comes back as
/// [`TarballError::ReadTarballEntries`] instead of panicking. Non-UTF-8
/// entry paths are coerced via [`std::path::Path::to_string_lossy`],
/// matching pnpm's string-based handling so a mixed install against the
/// shared `index.db` stays consistent; real-world npm tarballs are
/// UTF-8 so the coercion is almost never hit in practice.
fn extract_tarball_entries(
    archive: &mut Archive<Cursor<Vec<u8>>>,
    store_dir: &StoreDir,
) -> Result<(HashMap<String, PathBuf>, PackageFilesIndex), TarballError> {
    let entries = archive
        .entries()
        .map_err(TarballError::ReadTarballEntries)?
        // Keep only regular-file `Ok` entries; anything else in the
        // `Ok` arm (directories, symlinks, hardlinks, pax/gnu
        // extension headers, …) is dropped. `Err` entries fall
        // through so the `?` inside the loop below propagates them —
        // previously this branch did `entry.as_ref().unwrap()` which
        // panicked on any iterator-level error.
        .filter(|entry| match entry {
            Ok(entry) => entry.header().entry_type().is_file(),
            Err(_) => true,
        });

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
        let mut entry = entry.map_err(TarballError::ReadTarballEntries)?;

        let file_mode = entry.header().mode().map_err(TarballError::ReadTarballEntries)?;
        let file_is_executable = file_mode::is_executable(file_mode);

        // Read the contents of the entry. `entry.size()` is the size
        // from the tar header — untrusted input from the tarball, not
        // from any disk-side signal we've verified. Clamp the
        // pre-allocation hint so a corrupt or malicious tarball that
        // claims gigabytes can't turn `Vec::with_capacity` into an OOM
        // abort before `read_to_end` has a chance to surface the real
        // error. The claimed size beyond the clamp is still read
        // through `Vec`'s geometric growth. `try_reserve` propagates
        // an allocation failure as an I/O error rather than aborting.
        // 64 MiB is generous for any legitimate single-file entry in
        // an npm tarball — typical entries are well under 1 MiB.
        const MAX_ENTRY_PREALLOC_BYTES: u64 = 64 * 1024 * 1024;
        let prealloc_hint = entry.size().min(MAX_ENTRY_PREALLOC_BYTES) as usize;
        let mut buffer = Vec::new();
        buffer.try_reserve(prealloc_hint).map_err(|err| {
            TarballError::ReadTarballEntries(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                format!("failed to reserve {prealloc_hint} bytes for tar entry: {err}"),
            ))
        })?;
        entry.read_to_end(&mut buffer).map_err(TarballError::ReadTarballEntries)?;

        let entry_path = entry.path().map_err(TarballError::ReadTarballEntries)?;
        // `components().skip(1)` drops the top-level package
        // directory (`package/`). Every remaining component must be
        // `Component::Normal`: a hostile tarball can carry `..`,
        // absolute-root, or Windows-prefix components that — joined
        // onto the CAFS extraction root later in `create_cas_files`
        // — would land files outside the store (directory traversal).
        // Reject loudly rather than silently normalize so tampering
        // is visible.
        let mut cleaned = PathBuf::new();
        for component in entry_path.components().skip(1) {
            let Component::Normal(part) = component else {
                return Err(TarballError::ReadTarballEntries(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "tar entry path rejected (non-normal component, possible directory traversal): {entry_path:?}",
                    ),
                )));
            };
            cleaned.push(part);
        }
        if cleaned.as_os_str().is_empty() {
            return Err(TarballError::ReadTarballEntries(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "tar entry path has no payload after dropping the top-level component: {entry_path:?}",
                ),
            )));
        }
        // `to_string_lossy()` coerces non-UTF-8 bytes to U+FFFD —
        // matching pnpm's string-based path layer so a shared
        // `index.db` stays consistent across the two tools.
        let cleaned_entry_path = cleaned.to_string_lossy().into_owned();
        let (file_path, file_hash) = store_dir
            .write_cas_file(&buffer, file_is_executable)
            .map_err(TarballError::WriteCasFile)?;

        if let Some(previous) = cas_paths.insert(cleaned_entry_path.clone(), file_path) {
            tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
        }

        // `as_millis()` returns `u128`; narrow to `u64` to match the
        // store index schema — see `CafsFileInfo::checked_at` for why
        // `u64` is used. Using `u64::try_from` rather than `as u64`
        // avoids a silent wrap: even though millisecond epochs don't
        // overflow `u64` for ~584M years, the intent should be
        // explicit. If the clock ever reports something
        // unrepresentable, drop the timestamp — the `checkedAt` field
        // is optional and pnpm tolerates `None`.
        let checked_at = UNIX_EPOCH.elapsed().ok().and_then(|x| u64::try_from(x.as_millis()).ok());
        let file_size = entry.header().size().map_err(TarballError::ReadTarballEntries)?;
        let file_attrs = CafsFileInfo {
            digest: format!("{file_hash:x}"),
            mode: file_mode,
            size: file_size,
            checked_at,
        };

        if let Some(previous) = pkg_files_idx.files.insert(cleaned_entry_path, file_attrs) {
            tracing::warn!(?previous, "Duplication detected. Old entry has been ejected");
        }
    }

    Ok((cas_paths, pkg_files_idx))
}

/// Try to reconstruct the `{filename → CAFS path}` map for a package from
/// the SQLite store index, without going to the network. Returns `None`
/// if anything looks off — no index handed in, no row, unreadable row,
/// failed integrity check — so the caller falls through to a fresh
/// download.
///
/// The `verify_store_integrity` parameter matches pnpm's flag of the
/// same name. When `true` (pnpm's default) each referenced CAFS file is
/// stat'ed and compared against the stored `checkedAt`/size, with a
/// re-hash only when the mtime has advanced. When `false` the lookup
/// builds the filename→path map straight from the index row without any
/// filesystem work — missing / corrupt CAFS blobs surface lazily when
/// the caller tries to import them.
///
/// The previous pacquet implementation unconditionally ran a
/// `symlink_metadata` per referenced file and rejected any non-regular
/// dirent outright. That cost a stat syscall per file on every warm
/// install (#260) and still diverged from pnpm: the upstream
/// [`checkPkgFilesIntegrity`][1] catches corruption via the content hash
/// and doesn't gate on dirent type.
///
/// [1]: https://github.com/pnpm/pnpm/blob/main/store/cafs/src/checkPkgFilesIntegrity.ts
///
/// The `index` argument is a shared read-only handle that callers open
/// once per install and pass in repeatedly, so we don't pay the
/// `Connection::open` + PRAGMA cost per package.
async fn load_cached_cas_paths(
    index: Option<SharedReadonlyStoreIndex>,
    store_dir: &'static StoreDir,
    cache_key: String,
    verify_store_integrity: bool,
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

        let verify_result = if verify_store_integrity {
            pacquet_store_dir::check_pkg_files_integrity(store_dir, entry)
        } else {
            pacquet_store_dir::build_file_maps_from_index(store_dir, entry)
        };
        if !verify_result.passed {
            // Per-file reason (filename, CAS path, size mismatch, hash
            // mismatch, …) is logged at `debug!` inside
            // `check_pkg_files_integrity` / `build_file_maps_from_index`
            // where the failure actually happens — this caller-side log
            // just summarises "the row as a whole didn't verify" so log
            // scrapers can correlate the per-file debug lines with the
            // snapshot they belong to.
            tracing::debug!(
                target: "pacquet::download",
                ?cache_key,
                "store-index entry failed integrity check; re-fetching",
            );
            return None;
        }
        Some(verify_result.files_map)
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
    /// Handle to the batched store-index writer. Each successful tarball
    /// extraction queues one `(key, PackageFilesIndex)` row; a single
    /// writer task drains the channel and flushes batches of up to 256 in
    /// one transaction each, so the whole install goes through one
    /// `Connection::open` and a handful of WAL commits instead of the old
    /// "open + PRAGMA + insert + drop" per tarball (which ballooned
    /// tokio's blocking pool to 500+ threads on a 1352-snapshot install —
    /// see #263). `None` degrades to "skip index row", matching the read
    /// side's stance: install still succeeds, the next install misses on
    /// this cache key and re-downloads.
    pub store_index_writer: Option<Arc<StoreIndexWriter>>,
    /// Mirrors pnpm's `verify-store-integrity` / `verifyStoreIntegrity`
    /// setting. When `true` (pnpm's default) each cached CAFS file is
    /// stat'ed and optionally re-hashed before reuse. When `false` the
    /// index is trusted and the import fails lazily if a blob is
    /// missing — trades the per-file stat / optional rehash for the
    /// risk that a mutated or corrupt store serves stale content until
    /// the next integrity-full install. Whether that translates into a
    /// wall-time win depends on the workload; the per-snapshot stat
    /// isn't the bottleneck on the benchmarks this repo tracks (see
    /// #273), but cutting the syscall count is still correct.
    pub verify_store_integrity: bool,
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
            verify_store_integrity,
            ..
        } = self;
        let store_index = self.store_index.clone();
        let store_index_writer = self.store_index_writer.clone();

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
        if let Some(cas_paths) =
            load_cached_cas_paths(store_index, store_dir, cache_key, verify_store_integrity).await
        {
            tracing::info!(target: "pacquet::download", ?package_url, ?package_id, "Reusing cached CAFS entry — skipping download");
            return Ok(cas_paths);
        }

        tracing::info!(target: "pacquet::download", ?package_url, "New cache");

        let network_error = |error| {
            TarballError::FetchTarball(NetworkError { url: package_url.to_string(), error })
        };
        let response_head = http_client
            .run_with_permit(|client| client.get(package_url).send())
            .await
            .map_err(network_error)?;

        // Read `Content-Length` *before* we start consuming the body
        // stream so we can pre-size the buffer. Ports pnpm v11's
        // `fetching/tarball-fetcher/src/remoteTarballFetcher.ts:148-164`:
        // reqwest/hyper internally grows its buffer by doubling when
        // CL isn't used, so on a 1352-tarball cold install that's a
        // lot of wasted alloc + copy work across the pipeline.
        let expected_size = response_head.content_length();

        // Gate the memory-heavy + CPU-heavy part of the pipeline with
        // `post_download_semaphore`:
        //
        // - The blocking pool is 512-wide by default, which is right
        //   for I/O wait but disastrous for CPU work that can only
        //   really run `num_cpus` at a time, so we cap concurrent
        //   `spawn_blocking` bodies.
        // - We also acquire the permit *before* we consume the body
        //   rather than right before `spawn_blocking`. Buffering is
        //   where the per-tarball memory spike lives (a full
        //   decompressed package can be many MB), so holding the
        //   permit across buffering bounds the number of fully-buffered
        //   response bodies in RAM to the post-download cap. Without
        //   this, a fast registry + `try_join_all` fan-out could pile
        //   up hundreds of buffered tarballs waiting for a permit to
        //   process (Copilot review on #269).
        //
        // The permit is held across both the body-stream drain and
        // the `spawn_blocking.await` below, dropping at end of scope.
        let _post_download_permit = post_download_semaphore()
            .acquire()
            .await
            .expect("post-download semaphore shouldn't be closed this soon");

        // Stream the body into a single pre-sized `Vec<u8>` when
        // `Content-Length` is known. One allocation + one
        // `extend_from_slice` per chunk, no growth-by-doubling.
        // Falls back to empty capacity when CL is missing (chunked
        // transfer encoding), which still avoids reqwest/hyper's
        // intermediate-chunk-list + second-copy pass.
        //
        // We don't re-verify the received byte count against
        // `Content-Length` — hyper enforces CL framing itself on the
        // receive side (a body shorter than CL errors the stream, a
        // body longer is truncated or queued as the next request),
        // so the check would be dead code. Pnpm's equivalent
        // `BadTarballError` path exists because undici in Node.js
        // doesn't always enforce it.
        let response = {
            use futures_util::StreamExt;
            let mut buf = allocate_tarball_buffer(expected_size, package_url)?;
            let mut stream = response_head.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(network_error)?;
                buf.extend_from_slice(&chunk);
            }
            buf
        };

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
        // (`write_cas_file`). Running it as a plain `tokio::task::spawn`
        // pinned a tokio reactor worker for the entirety of each tarball
        // — on a 2-core runner that meant at most two tarballs could
        // make progress at a time, and the tail of large packages
        // missed the CI step budget even though the network side was
        // long done. `spawn_blocking` uses tokio's dedicated
        // blocking-thread pool instead, so the tail drains in parallel.
        // The store-index row handoff at the end stays non-blocking
        // (`StoreIndexWriter::queue`, #265), so the closure itself does
        // no SQLite work. Concurrency is already capped by the
        // `_post_download_permit` acquired above.
        let cas_paths =
            tokio::task::spawn_blocking(move || -> Result<HashMap<String, PathBuf>, TaskError> {
                package_integrity.check(&response).map_err(TaskError::Checksum)?;

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
                    extract_tarball_entries(&mut archive, store_dir)?
                };

                // Hand the per-tarball files index off to the shared
                // writer task from #265. `queue` is a non-blocking
                // `UnboundedSender::send` — no SQLite work on this thread,
                // just a channel push; the writer task owns one
                // connection and batches whatever it drains in one
                // `BEGIN IMMEDIATE; … ; COMMIT`. `None` means the writer
                // failed to open or the caller handed us none — the row
                // is dropped with a `warn!` and the next install misses
                // on this cache key, matching the read path's stance.
                let index_key = store_index_key(&package_integrity.to_string(), &package_id);
                if let Some(writer) = store_index_writer {
                    writer.queue(index_key, pkg_files_idx);
                } else {
                    tracing::warn!(
                        target: "pacquet::download",
                        ?index_key,
                        "no shared store-index writer; skipping index row for this tarball",
                    );
                }

                Ok(cas_paths)
            })
            .await
            .map_err(TarballError::TaskJoin)?
            .map_err(|error| match error {
                TaskError::Checksum(error) => TarballError::Checksum(VerifyChecksumError {
                    url: package_url.to_string(),
                    error,
                }),
                TaskError::Other(error) => error,
            })?;

        tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

        Ok(cas_paths)
    }
}

#[cfg(test)]
mod tests {
    use pacquet_store_dir::StoreIndex;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use tempfile::{tempdir, TempDir};

    use super::*;

    fn integrity(integrity_str: &str) -> Integrity {
        integrity_str.parse().expect("parse integrity string")
    }

    /// Absent `Content-Length` (chunked transfer) returns an empty
    /// growable buffer. The stream loop extends it as chunks arrive.
    #[test]
    fn allocate_tarball_buffer_returns_empty_when_content_length_is_absent() {
        let buf = allocate_tarball_buffer(None, "https://example.test/pkg.tgz")
            .expect("no content-length is a valid chunked-transfer response");
        assert_eq!(buf.len(), 0);
    }

    /// Reasonable `Content-Length` pre-sizes the buffer so no
    /// realloc happens during the stream loop. `try_reserve_exact`
    /// succeeds; we don't assert `buf.capacity() == size` because
    /// allocators are allowed to round up, only that it's at least
    /// what we asked for.
    #[test]
    fn allocate_tarball_buffer_presizes_for_reasonable_content_length() {
        let buf = allocate_tarball_buffer(Some(1024 * 1024), "https://example.test/pkg.tgz")
            .expect("1 MiB pre-allocation should succeed on any dev / CI box");
        assert!(buf.capacity() >= 1024 * 1024, "capacity = {}", buf.capacity());
        assert_eq!(buf.len(), 0);
    }

    /// A maliciously or buggily huge `Content-Length` must not be
    /// passed through to the infallible `Vec::with_capacity` — that
    /// would abort the process on allocation failure. `try_reserve_exact`
    /// surfaces the failure as `TarballTooLarge` so the install can
    /// reject this one package and continue.
    #[test]
    fn allocate_tarball_buffer_rejects_absurd_content_length() {
        let url = "https://example.test/evil.tgz";
        let err = allocate_tarball_buffer(Some(u64::MAX), url)
            .expect_err("u64::MAX cannot actually be reserved");
        match err {
            TarballError::TarballTooLarge { url: got_url, advertised_size } => {
                assert_eq!(got_url, url);
                assert_eq!(advertised_size, u64::MAX);
            }
            other => panic!("expected TarballTooLarge, got {other:?}"),
        }
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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
            store_index_writer: None,
            verify_store_integrity: true,
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

    /// The per-entry loop used to be a pile of `.unwrap()` /
    /// `.expect()` calls that turned any tar-side failure — corrupt
    /// header, short body read, path decode — into a panic inside a
    /// blocking-pool task (which took the whole install with it and
    /// occasionally left the pool with dangling permits). The loop now
    /// lives in `extract_tarball_entries` and propagates every such
    /// failure as [`TarballError::ReadTarballEntries`]. This test
    /// feeds the function bytes that aren't a valid tar archive and
    /// asserts we get that error rather than a panic.
    ///
    /// We don't invoke `decompress_gzip` here: the decompression layer
    /// has its own error path and isn't the code under test. Driving
    /// `extract_tarball_entries` directly isolates the tar iterator's
    /// failure modes.
    #[test]
    fn extract_propagates_malformed_tar_instead_of_panicking() {
        let (tempdir, store_path) = tempdir_with_leaked_path();

        // 1 KiB of 0xFF: not a tar header (checksum at bytes 148..156
        // can't possibly match), so the iterator either yields an
        // `Err` on the first entry or errors on path decode. Either
        // way the filter+map_err plumbing must surface the failure as
        // `TarballError::ReadTarballEntries`.
        let bogus: Vec<u8> = vec![0xFF; 1024];
        let mut archive = Archive::new(Cursor::new(bogus));

        let err = extract_tarball_entries(&mut archive, store_path)
            .expect_err("malformed tar must surface a TarballError, not panic");

        assert!(
            matches!(err, TarballError::ReadTarballEntries(_)),
            "expected ReadTarballEntries, got: {err:?}"
        );

        drop(tempdir);
    }

    /// A tarball whose entry path contains `..` (or any other
    /// non-`Normal` path component) must be rejected, not silently
    /// normalized. Without the guard in `extract_tarball_entries`,
    /// `cleaned_entry_path` would later be joined onto the CAFS
    /// extraction root by `create_cas_files` and land files outside
    /// the store (directory traversal).
    ///
    /// Note: `tar::Header::set_path` refuses to write a `..` path on
    /// its own (defense in depth on the write side). To exercise the
    /// read-side guard we have to bypass that by writing the name
    /// bytes directly via `as_mut_bytes()` and recomputing the
    /// checksum. A malicious tarball in the wild could trivially be
    /// written by any non-Rust tool that doesn't sanitize.
    #[test]
    fn extract_rejects_parent_dir_component_in_entry_path() {
        let (tempdir, store_path) = tempdir_with_leaked_path();

        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // Bypass `set_path`'s `..` validation: write the raw
            // name bytes directly into header[0..100]. Then
            // `set_cksum()` recomputes the checksum over those bytes
            // so the reader doesn't trip its own integrity check.
            let raw = header.as_mut_bytes();
            let name = b"package/../evil.txt";
            raw[..name.len()].copy_from_slice(name);
            for b in &mut raw[name.len()..100] {
                *b = 0;
            }
            header.set_cksum();
            builder.append(&header, &b"evil!"[..]).expect("append entry");
            builder.finish().expect("finalize tar");
        }

        let mut archive = Archive::new(Cursor::new(tar_bytes));
        let err = extract_tarball_entries(&mut archive, store_path)
            .expect_err("parent-dir component must be rejected, not normalized");

        match err {
            TarballError::ReadTarballEntries(io_err) => {
                assert_eq!(io_err.kind(), std::io::ErrorKind::InvalidData);
            }
            other => panic!("expected ReadTarballEntries(InvalidData), got: {other:?}"),
        }

        drop(tempdir);
    }
}
