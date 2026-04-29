use std::{
    collections::HashMap,
    io::{Cursor, Read},
    path::{Component, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, UNIX_EPOCH},
};

use dashmap::DashMap;
use derive_more::{Display, Error, From};
use miette::Diagnostic;
use pacquet_fs::file_mode;
use pacquet_network::{AuthHeaders, ThrottledClient};
use pacquet_store_dir::{
    CafsFileInfo, PackageFilesIndex, SharedReadonlyStoreIndex, SharedVerifiedFilesCache, StoreDir,
    StoreIndexError, StoreIndexWriter, WriteCasFileError, store_index_key,
};
use pipe_trait::Pipe;
use smart_default::SmartDefault;
use ssri::Integrity;
use tar::Archive;
use tokio::sync::{Notify, RwLock, Semaphore};
use tracing::instrument;
use zune_inflate::{DeflateDecoder, DeflateOptions, errors::InflateDecodeErrors};

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

/// Reqwest's own [`std::fmt::Display`] for a request-stage failure renders as
/// `error sending request for url (URL): <inner>` only if it can find
/// an inner source, and on some failure modes (e.g. the request was
/// dropped before a connect was attempted) `inner` is `None` —
/// leaving the user with the truly opaque `error sending request for
/// url (URL)` and no clue about what actually failed.
///
/// `walk_reqwest_chain` walks `error.source()` itself and joins every
/// stage's `Display` with `: ` so the rendered `NetworkError` always
/// carries the leaf reason (e.g. `Connection refused (os error 61)`,
/// `tls handshake eof`, `dns error: failed to lookup address`),
/// regardless of which intermediate `reqwest` / `hyper` / `io::Error`
/// happens to elide it.
fn walk_reqwest_chain(error: &reqwest::Error) -> String {
    let mut out = error.to_string();
    let mut error: &dyn std::error::Error = error;
    while let Some(src) = error.source() {
        let s = src.to_string();
        // Skip empty or duplicate frames — hyper occasionally repeats
        // the same message across two layers, and reqwest sometimes
        // already includes the inner string in its top-level Display.
        if !s.is_empty() && !out.ends_with(&s) {
            out.push_str(": ");
            out.push_str(&s);
        }
        error = src;
    }
    out
}

/// Settings for the per-fetch retry loop. Mirrors pnpm's
/// `fetch-retries` / `fetch-retry-factor` /
/// `fetch-retry-mintimeout` / `fetch-retry-maxtimeout` and the
/// `@zkochan/retry` algorithm pnpm uses in
/// `network/fetch/src/fetch.ts`:
///
/// `delay = min(min_timeout * factor.pow(attempt), max_timeout)`
///
/// `attempt` is zero-indexed, so the first post-failure wait is
/// `min_timeout`. `retries` is the number of *retries* — total
/// attempts is `retries + 1`.
///
/// # Pathological configurations
///
/// We don't sanitize these here because pnpm doesn't either —
/// the config plumbing is meant to be byte-equivalent to upstream.
/// The total number of attempts is always bounded by `retries`, so
/// even a degenerate `delay_for` only removes the backoff:
///
/// * `factor == 0` keeps the first wait at `min_timeout` (`0u32.pow(0)
///   == 1`), but every subsequent wait is `0` — i.e. no backoff
///   between retries. Same as pnpm.
/// * `factor == 1` waits `min_timeout` between every attempt. Same as
///   pnpm.
/// * `max_timeout < min_timeout` makes every wait `max_timeout`. Same
///   as pnpm.
///
/// If a caller wants stricter validation (warn / reject these
/// configs), it belongs above the `Npmrc` boundary, alongside any
/// other npmrc sanity checks pnpm grows over time.
///
/// Defaults (via [`SmartDefault`]) match pnpm's
/// [`config/reader/src/index.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/config/reader/src/index.ts#L146-L149)
/// (2 retries, factor 10, 10 s floor, 60 s cap).
#[derive(Debug, Clone, Copy, SmartDefault)]
pub struct RetryOpts {
    #[default = 2]
    pub retries: u32,
    #[default = 10]
    pub factor: u32,
    #[default(Duration::from_millis(10_000))]
    pub min_timeout: Duration,
    #[default(Duration::from_millis(60_000))]
    pub max_timeout: Duration,
}

impl RetryOpts {
    /// Backoff to wait before the `(attempt + 1)`-th attempt, where
    /// `attempt` is the zero-indexed number of failures so far.
    /// Matches `@zkochan/retry`'s formula with `randomize: false`.
    fn delay_for(self, attempt: u32) -> Duration {
        // `Duration::as_millis` returns `u128` because a `Duration` can
        // hold values that overflow `u64` milliseconds, but
        // `Duration::from_millis` only takes `u64`. Saturate on the way
        // down so a pathological caller-supplied timeout produces the
        // largest expressible delay rather than a silently truncated one.
        let min_ms = self.min_timeout.as_millis().pipe(u64::try_from).unwrap_or(u64::MAX);
        let max_ms = self.max_timeout.as_millis().pipe(u64::try_from).unwrap_or(u64::MAX);
        let factor = u64::from(self.factor);
        let pow = factor.checked_pow(attempt).unwrap_or(u64::MAX);
        let ms = min_ms.saturating_mul(pow);
        let capped = ms.min(max_ms);
        Duration::from_millis(capped)
    }
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Failed to fetch {url}: {}", walk_reqwest_chain(error))]
pub struct NetworkError {
    pub url: String,
    /// Marked `#[error(source)]` so miette can also walk the chain on
    /// its own (some renderers prefer the structured form). The
    /// flattened string in `Display` is for the default miette report
    /// where the user just sees one line per wrapper.
    #[error(source)]
    pub error: reqwest::Error,
}

#[derive(Debug, Display, Error, Diagnostic)]
#[display("Tarball server returned HTTP {status} for {url}")]
pub struct HttpStatusError {
    pub url: String,
    pub status: u16,
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

    #[diagnostic(code(pacquet_tarball::http_status))]
    HttpStatus(HttpStatusError),

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
/// [1]: https://github.com/pnpm/pnpm/blob/1819226b51/store/cafs/src/checkPkgFilesIntegrity.ts
///
/// Pre-fetched cas-paths map shared across all per-snapshot futures.
/// Built once at install start by [`prefetch_cas_paths`]; downloads
/// consult it before falling through to a per-snapshot SQLite lookup.
///
/// Values are `Arc`-wrapped so the cold-batch fallback can hand a hit
/// back as a cheap pointer-clone rather than memcpy-ing the whole
/// per-file map (each entry is a `HashMap<String, PathBuf>` with up
/// to ~hundred entries, and Copilot reasonably flagged the deep clone
/// as a hot-path cost).
pub type PrefetchedCasPaths = HashMap<String, Arc<HashMap<String, PathBuf>>>;

/// Batch the entire warm-cache lookup phase into one `spawn_blocking`
/// task at install start: collect every row the lockfile is going to
/// ask about under a single `index.lock()` round-trip, drop the lock,
/// then run the per-package integrity checks unlocked. Returns a
/// `cache_key → Arc<cas_paths>` map the per-snapshot futures can hit
/// synchronously.
///
/// **Locking shape (per Copilot review on #292):** the SQLite mutex
/// is held only for the SELECT loop. Integrity checks (`fs::metadata`
/// per file, optional re-hash) happen after the guard drops, so a
/// concurrent reader on the same `SharedReadonlyStoreIndex` doesn't
/// have to wait through the whole batch's filesystem work.
///
/// **Why one batched task instead of 1352 spawn_blockings:** the
/// per-snapshot path fans out one `tokio::task::spawn_blocking` per
/// snapshot. With 1352 snapshots all firing into the default
/// 512-thread blocking pool, threads compete for CPU and get
/// preempted between fs ops — sample-profiling showed cache-lookup
/// bodies averaging 20-60 ms each (sum 26-82 s) almost entirely
/// blocked, even though the actual SELECT (≈40 µs) and per-file
/// integrity stats (≈ms each) shouldn't take that long. Doing the
/// whole batch on one thread avoids the OS-scheduler / kernel-journal
/// thrash and makes each query fast in CPU-time. Pnpm's piscina pool
/// achieves the same shape implicitly with 4 dedicated workers.
///
/// Cache misses (no row, malformed row, integrity-check failure)
/// just don't appear in the result. The caller then falls through
/// to [`DownloadTarballToStore::run_without_mem_cache`] for those
/// keys, which still has its own cache check as a backstop.
pub async fn prefetch_cas_paths(
    index: Option<SharedReadonlyStoreIndex>,
    store_dir: &'static StoreDir,
    cache_keys: Vec<String>,
    verify_store_integrity: bool,
    verified_files_cache: SharedVerifiedFilesCache,
) -> PrefetchedCasPaths {
    let Some(index) = index else { return HashMap::new() };
    if cache_keys.is_empty() {
        return HashMap::new();
    }
    let result = tokio::task::spawn_blocking(move || -> PrefetchedCasPaths {
        // Phase 1: read every row under the mutex; drop the guard
        // before running any filesystem work. One batched
        // `SELECT … WHERE key IN (?, ?, …)` per `GET_MANY_CHUNK`
        // (see `StoreIndex::get_many`) collapses what used to be N
        // round-trips into one — see #294 for the cold-cache regression
        // the per-key loop introduced when every key missed.
        let entries: HashMap<String, PackageFilesIndex> = {
            let Ok(guard) = index.lock() else {
                tracing::debug!(
                    target: "pacquet::download",
                    "store-index mutex poisoned at prefetch start; falling back to per-snapshot lookups",
                );
                return HashMap::new();
            };
            match guard.get_many(&cache_keys) {
                Ok(map) => map,
                Err(error) => {
                    tracing::debug!(
                        target: "pacquet::download",
                        ?error,
                        "store-index batched read failed at prefetch start; falling back to per-snapshot lookups",
                    );
                    return HashMap::new();
                }
            }
        };
        // Phase 2: integrity-check each entry without holding the lock.
        let mut out = HashMap::with_capacity(entries.len());
        for (cache_key, entry) in entries {
            let verify_result = if verify_store_integrity {
                pacquet_store_dir::check_pkg_files_integrity(
                    store_dir,
                    entry,
                    &verified_files_cache,
                )
            } else {
                pacquet_store_dir::build_file_maps_from_index(store_dir, entry)
            };
            if verify_result.passed {
                out.insert(cache_key, Arc::new(verify_result.files_map));
            }
        }
        out
    })
    .await;
    result.unwrap_or_else(|error| {
        tracing::warn!(
            target: "pacquet::download",
            ?error,
            "store-index prefetch task failed; falling back to per-snapshot lookups",
        );
        HashMap::new()
    })
}

/// The `index` argument is a shared read-only handle that callers open
/// once per install and pass in repeatedly, so we don't pay the
/// `Connection::open` + PRAGMA cost per package.
async fn load_cached_cas_paths(
    index: Option<SharedReadonlyStoreIndex>,
    store_dir: &'static StoreDir,
    cache_key: String,
    verify_store_integrity: bool,
    verified_files_cache: SharedVerifiedFilesCache,
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
            pacquet_store_dir::check_pkg_files_integrity(store_dir, entry, &verified_files_cache)
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
    /// Install-scoped dedup cache shared across every cached-tarball
    /// lookup. Ports pnpm's `verifiedFilesCache: Set<string>`: a CAFS
    /// path that one snapshot's verify pass has already stat'ed (and
    /// optionally re-hashed) gets skipped when the next snapshot
    /// touches the same blob. Without it pacquet was paying the
    /// per-file stat in `check_pkg_files_integrity` once per
    /// (snapshot × file) instead of once per (file). Allocate one
    /// `Arc<DashSet<PathBuf>>` at install bootstrap and pass the same
    /// handle to every `DownloadTarballToStore`.
    pub verified_files_cache: SharedVerifiedFilesCache,
    pub package_integrity: &'a Integrity,
    pub package_unpacked_size: Option<usize>,
    pub package_url: &'a str,
    /// Stable identifier for the package, e.g. `"{name}@{version}"`. Paired
    /// with `package_integrity` to form the SQLite index key per pnpm v11's
    /// `storeIndexKey`.
    pub package_id: &'a str,
    /// URL-keyed `Authorization` header lookup, built from the parsed
    /// `.npmrc` creds. Resolved per request so a tarball served from a
    /// different host than the registry still picks up its own header.
    /// Mirrors pnpm's
    /// [`getAuthHeaderByURI`](https://github.com/pnpm/pnpm/blob/601317e7a3/network/auth-header/src/index.ts)
    /// pattern.
    pub auth_headers: &'a AuthHeaders,
    /// Pre-fetched cache lookups built once at install start
    /// ([`prefetch_cas_paths`]). When `Some`, this is consulted first;
    /// the per-snapshot SQLite + integrity-check round-trip is skipped
    /// for every key already resolved by the prefetch.
    pub prefetched_cas_paths: Option<&'a PrefetchedCasPaths>,
    /// Per-attempt retry budget for the tarball pipeline. Mirrors pnpm's
    /// `fetch-retries*` knobs (`network/fetch/src/fetch.ts`,
    /// `fetching/tarball-fetcher/src/remoteTarballFetcher.ts`): every
    /// failure retries except HTTP 401, 403, 404 — including arbitrary
    /// 4xx / 5xx, network resets, timeouts, mid-stream body errors,
    /// integrity mismatches, and gzip / tar parse failures (#259).
    pub retry_opts: RetryOpts,
}

/// Whether a [`TarballError`] from one tarball-fetch attempt should be
/// retried. Matches pnpm's
/// [`remoteTarballFetcher.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/fetching/tarball-fetcher/src/remoteTarballFetcher.ts#L76-L84)
/// policy *exactly*: only HTTP 401, 403, 404 (and the git-prepare
/// failure code, which doesn't apply to registry tarballs) fail fast.
/// Every other failure — arbitrary 4xx, 5xx, network reset, timeout,
/// integrity mismatch, gzip / tar parse error, CAFS write hiccup —
/// retries until the budget is exhausted.
///
/// In particular this means we retry integrity mismatches and decode
/// errors. pnpm wraps the body fetch *and* the post-download
/// `addFilesFromTarball` (integrity check + extraction) in one retried
/// closure for the same reason: a corrupted byte on the wire that
/// happens to escape TCP framing can break either the integrity check
/// or the gzip decode, and a re-fetch is the cheapest way out.
fn is_transient_error(err: &TarballError) -> bool {
    match err {
        TarballError::HttpStatus(http) => !matches!(http.status, 401 | 403 | 404),
        _ => true,
    }
}

/// Run one full tarball-fetch attempt: hit the network, drain the body
/// into RAM, verify the integrity hash, then decompress and extract
/// every entry into the CAFS. Returns the cas-paths map and the
/// per-tarball [`PackageFilesIndex`] row that the caller queues into
/// the shared store-index writer once the retry loop succeeds.
///
/// The whole pipeline lives in one attempt because pnpm's tarball
/// fetcher does the same: any failure inside `addFilesFromTarball`
/// (integrity mismatch, gzip decode, malformed tar) propagates back
/// to the retry boundary so a re-fetch can recover from a flaky
/// transfer that happens to checksum or decode wrong.
///
/// Permits are acquired *inside* this function so a backoff sleep
/// between attempts doesn't keep one parked. The network permit is
/// held from `connect + send` through body streaming (matching pnpm's
/// pQueue and #281's EMFILE fix), then dropped before the
/// `post_download_semaphore` permit gates the CPU-bound checksum +
/// decode + extract step.
async fn fetch_and_extract_once(
    http_client: &ThrottledClient,
    package_url: &str,
    package_integrity: &Integrity,
    package_unpacked_size: Option<usize>,
    store_dir: &'static StoreDir,
    auth_headers: &AuthHeaders,
) -> Result<(HashMap<String, PathBuf>, PackageFilesIndex), TarballError> {
    let network_error =
        |error| TarballError::FetchTarball(NetworkError { url: package_url.to_string(), error });

    // Acquire the network permit *before* `connect + send` and hold it
    // through body streaming. Releasing earlier would let the next
    // batch of futures `connect()` while previous bodies are still
    // draining, breaking the bound on concurrent open sockets.
    let client = http_client.acquire().await;
    let mut request = client.get(package_url);
    // Match pnpm's tarball download path
    // ([`remoteTarballFetcher.ts`](https://github.com/pnpm/pnpm/blob/601317e7a3/fetching/tarball-fetcher/src/remoteTarballFetcher.ts#L66-L70)):
    // resolve the per-URL auth header and attach it. Tarball hosts that
    // differ from the metadata host still pick up the header keyed at
    // the registry's nerf-darted URI.
    if let Some(value) = auth_headers.for_url(package_url) {
        request = request.header("authorization", value);
    }
    let response_head = request.send().await.map_err(network_error)?;

    let status = response_head.status();
    if !status.is_success() {
        // Drain small error bodies so reqwest/hyper can return the
        // connection to the keep-alive pool — dropping an unconsumed
        // `Response` closes the underlying connection, which we'd then
        // pay to reopen on retry. Skip the drain when the body is
        // unknown-length or larger than the cap, since hyper only
        // returns the connection to the pool once the body is fully
        // consumed; a partial drain wouldn't help and would just buffer
        // a pathological response.
        const DRAIN_CAP: u64 = 64 * 1024;
        if response_head.content_length().is_some_and(|len| len <= DRAIN_CAP) {
            let _ = response_head.bytes().await;
        }
        return Err(TarballError::HttpStatus(HttpStatusError {
            url: package_url.to_string(),
            status: status.as_u16(),
        }));
    }

    let expected_size = response_head.content_length();

    let buffer = {
        use futures_util::StreamExt;
        let mut buf = allocate_tarball_buffer(expected_size, package_url)?;
        let mut stream = response_head.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(network_error)?;
            buf.extend_from_slice(&chunk);
        }
        buf
    };

    // Body fully buffered; release the network permit before the
    // CPU-bound work so spawn_blocking doesn't hold one of the
    // limited fetch slots.
    //
    // The network permit was the only gate during fetch + body
    // buffering — `default_network_concurrency()` bounds concurrent
    // open sockets and concurrent in-progress fetches. The buffer
    // lives in RAM across this drop and the next acquire, so a
    // pathologically slow decompression stage could let buffered
    // tarballs accumulate beyond the network bound. In practice
    // flate2 decompresses faster than the network delivers, so
    // buffered-but-not-yet-decompressing tarballs stay close to zero.
    // Gating body buffering with `post_download_semaphore` (the
    // smaller `num_cpus * 2` cap) instead would pin `network_concurrency`
    // permits waiting for it and collapse fetch concurrency down to
    // `post_download` — that's the regression `perf(tarball)` (a43ca32)
    // fixed; don't reintroduce it.
    drop(client);

    // Gate the CPU-heavy decompress + cafs-write pipeline. The blocking
    // pool is 512-wide by default, which is right for I/O wait but
    // disastrous for CPU work that can only really run `num_cpus` at a
    // time, so we cap concurrent `spawn_blocking` bodies. The permit is
    // held across the `spawn_blocking.await` below and dropped at end
    // of scope.
    let _post_download_permit = post_download_semaphore()
        .acquire()
        .await
        .expect("post-download semaphore shouldn't be closed this soon");

    tracing::info!(target: "pacquet::download", ?package_url, "Download completed");

    // Move the CPU-bound work (SHA-512, gzip inflate, per-file SHA-512,
    // CAFS writes) onto the blocking pool. Same reasoning as before the
    // retry refactor: a plain `tokio::spawn` pinned a reactor worker for
    // each tarball — on a 2-core runner only two tarballs could make
    // progress at a time. The post-download semaphore caps concurrency
    // here.
    let package_integrity = package_integrity.clone();
    let package_url_owned = package_url.to_string();
    let result = tokio::task::spawn_blocking(
        move || -> Result<(HashMap<String, PathBuf>, PackageFilesIndex), TarballError> {
            package_integrity.check(&buffer).map_err(|error| {
                TarballError::Checksum(VerifyChecksumError { url: package_url_owned, error })
            })?;

            // Extract in a scope so the decompressed buffer + `tar::Archive`
            // are released before we return — a large package's inflated
            // bytes can be many MB.
            let (cas_paths, pkg_files_idx) = {
                let mut archive = decompress_gzip(&buffer, package_unpacked_size)?
                    .pipe(Cursor::new)
                    .pipe(Archive::new);
                extract_tarball_entries(&mut archive, store_dir)?
            };
            Ok((cas_paths, pkg_files_idx))
        },
    )
    .await
    .map_err(TarballError::TaskJoin)??;

    tracing::info!(target: "pacquet::download", ?package_url, "Checksum verified");

    Ok(result)
}

/// Run [`fetch_and_extract_once`] under pnpm's retry policy. Permanent
/// errors (HTTP 401 / 403 / 404 — see [`is_transient_error`]) fail on
/// the first attempt; everything else sleeps with exponential backoff
/// and tries again until the budget is exhausted, surfacing the most
/// recent error.
///
/// On retry, CAFS writes from a previous attempt that may have made it
/// part-way through extraction stay on disk. That's safe: the CAFS is
/// content-addressed, so re-extracting the same bytes produces
/// identical paths and `write_cas_file` is idempotent.
async fn fetch_and_extract_with_retry(
    http_client: &ThrottledClient,
    package_url: &str,
    package_integrity: &Integrity,
    package_unpacked_size: Option<usize>,
    store_dir: &'static StoreDir,
    retry_opts: RetryOpts,
    auth_headers: &AuthHeaders,
) -> Result<(HashMap<String, PathBuf>, PackageFilesIndex), TarballError> {
    let mut attempt: u32 = 0;
    loop {
        let result = fetch_and_extract_once(
            http_client,
            package_url,
            package_integrity,
            package_unpacked_size,
            store_dir,
            auth_headers,
        )
        .await;
        match result {
            Ok(value) => return Ok(value),
            Err(err) if !is_transient_error(&err) => return Err(err),
            Err(err) if attempt >= retry_opts.retries => {
                tracing::warn!(
                    target: "pacquet::download",
                    ?package_url,
                    attempts = attempt + 1,
                    ?err,
                    "Tarball fetch retry budget exhausted",
                );
                return Err(err);
            }
            Err(err) => {
                let delay = retry_opts.delay_for(attempt);
                tracing::warn!(
                    target: "pacquet::download",
                    ?package_url,
                    attempt = attempt + 1,
                    max_attempts = retry_opts.retries + 1,
                    ?delay,
                    ?err,
                    "Tarball fetch failed; retrying after backoff",
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
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

        // `DashMap::get` returns a `Ref` that holds a shard read guard for
        // its entire lifetime. Holding it across `.await` deadlocks: while
        // this task is parked, another task on the same worker can call
        // `mem_cache.insert` for a key that hashes to the same shard,
        // block on the write side, and starve every worker. Clone the
        // inner `Arc` out and drop the `Ref` immediately.
        let existing = mem_cache.get(package_url).map(|r| Arc::clone(r.value()));
        if let Some(cache_lock) = existing {
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
            prefetched_cas_paths,
            retry_opts,
            auth_headers,
            ..
        } = self;
        let store_index = self.store_index.clone();
        let store_index_writer = self.store_index_writer.clone();
        let verified_files_cache = Arc::clone(&self.verified_files_cache);

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
        // Hot path on warm installs: the install-scoped `prefetch_cas_paths`
        // task already ran one batched SELECT + integrity-check pass for
        // every (integrity, pkg_id) the lockfile mentions. If our key is
        // there, the per-snapshot future skips both the SQLite round-trip
        // and the per-file stat work.
        //
        // We still deep-clone the inner per-file `HashMap` here because
        // `run_without_mem_cache` returns an owned `HashMap<…, …>`;
        // `(**cas_paths).clone()` walks every entry and clones each
        // `String`/`PathBuf`, not the `Arc`. The Arc wrapping in
        // `PrefetchedCasPaths` is what saves the deep clone on the *new*
        // warm-batch path in `create_virtual_store::run` (which uses
        // `cas_paths.as_ref()` to borrow the inner map directly); this
        // fallback path is the per-snapshot tokio-future flow which
        // only fires for cache-miss snapshots, where the deep clone
        // cost is dwarfed by the cold download that would otherwise
        // run. Propagating the `Arc` through this signature would
        // require a wider refactor of `DownloadTarballToStore`'s
        // return type.
        if let Some(prefetched) = prefetched_cas_paths
            && let Some(cas_paths) = prefetched.get(&cache_key)
        {
            tracing::info!(
                target: "pacquet::download",
                ?package_url,
                ?package_id,
                "Reusing prefetched CAFS entry — skipping download",
            );
            return Ok((**cas_paths).clone());
        }
        if let Some(cas_paths) = load_cached_cas_paths(
            store_index,
            store_dir,
            cache_key,
            verify_store_integrity,
            verified_files_cache,
        )
        .await
        {
            tracing::info!(target: "pacquet::download", ?package_url, ?package_id, "Reusing cached CAFS entry — skipping download");
            return Ok(cas_paths);
        }

        tracing::info!(target: "pacquet::download", ?package_url, "New cache");

        // Run the full fetch + integrity + extract pipeline under
        // pnpm's retry policy. Mirrors
        // [`remoteTarballFetcher.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/fetching/tarball-fetcher/src/remoteTarballFetcher.ts):
        // a single retried closure wraps both the network side and the
        // `addFilesFromTarball` side, so a flaky transfer that survives
        // TCP framing but fails the SHA-512 hash or trips gzip / tar
        // parsing recovers via re-fetch instead of aborting the install
        // (#259). Only HTTP 401 / 403 / 404 fail fast — see
        // [`is_transient_error`].
        let (cas_paths, pkg_files_idx) = fetch_and_extract_with_retry(
            http_client,
            package_url,
            package_integrity,
            package_unpacked_size,
            store_dir,
            retry_opts,
            auth_headers,
        )
        .await?;

        // Hand the per-tarball files index off to the shared writer task
        // from #265 *after* the retry loop returns, so transient failures
        // don't queue a half-built row that a successful retry would
        // duplicate. `queue` is a non-blocking `UnboundedSender::send`;
        // the writer task owns one connection and batches whatever it
        // drains in one `BEGIN IMMEDIATE; … ; COMMIT`. `None` means the
        // writer failed to open or the caller handed us none — the row
        // is dropped with a `warn!` and the next install misses on this
        // cache key, matching the read path's stance.
        let index_key = store_index_key(&package_integrity.to_string(), package_id);
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
    }
}

#[cfg(test)]
mod tests;
