use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

/// SQLite-backed per-package index that pnpm v11 stores alongside the CAFS
/// blobs. In the pacquet layout the file lives at
/// `<store-root>/v11/index.db` — call [`StoreIndex::open_in`] with a
/// [`StoreDir`] to hit that path, or [`StoreIndex::open`] with any directory
/// to drop `index.db` right inside it (used by tests and tools).
///
/// Each row keys a package by its tarball integrity plus a package identifier
/// and stores a msgpack-encoded [`PackageFilesIndex`]. The schema and PRAGMAs
/// below mirror pnpm's implementation in
/// [`store/index/src/index.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/store/index/src/index.ts)
/// so that the two tools can read each other's entries.
pub struct StoreIndex {
    conn: Connection,
}

/// Shared handle to a read-only [`StoreIndex`] that can be cheaply cloned and
/// sent across blocking tasks. SQLite's `Connection` is `Send` but not
/// `Sync`, so the `Mutex` gates concurrent reads to a single query at a time
/// — fine for our workload where every caller serializes one short query and
/// then hands off to per-file work without holding the lock.
pub type SharedReadonlyStoreIndex = Arc<Mutex<StoreIndex>>;

/// Handle producers use to hand rows off to the batched writer task. Clone
/// cheaply via [`Arc`] to share across tokio tasks.
///
/// The design mirrors pnpm's `queueWrites` / `flush` / `setRawMany` pattern
/// (see `store/index/src/index.ts`): producers don't touch SQLite, they
/// just push `(key, value)` onto an unbounded channel. A single
/// [`spawn_blocking`][tokio::task::spawn_blocking] task drains the channel,
/// collects each non-blocking burst into a batch (capped at 256 entries —
/// see `MAX_BATCH_SIZE`), and flushes it with one `BEGIN IMMEDIATE` …
/// `COMMIT`. That turns the per-snapshot `Connection::open` + 7-PRAGMA +
/// solo-INSERT pattern into one open + N transactions, amortizes the WAL
/// commit fsync across the batch, and leaves tokio's blocking pool alone
/// (one writer thread, not one per tarball).
pub struct StoreIndexWriter {
    tx: tokio::sync::mpsc::UnboundedSender<(String, PackageFilesIndex)>,
    /// One-shot log guard for the "channel closed" case in [`Self::queue`].
    /// A dead writer (task panicked, [`StoreIndex::open`] failed) means
    /// every subsequent `queue` call fails — without this guard that
    /// spams 1352+ identical warnings into the install log. Once the
    /// first failure has been logged, further failures go silent;
    /// subsequent installs will still observe the missing index rows
    /// and re-download, which is the only actionable signal anyway.
    warn_on_send_failure: AtomicBool,
}

/// Batch cap for [`StoreIndexWriter`]. Big enough that a 1352-snapshot
/// install flushes in a handful of transactions (so the fsync cost is
/// amortized), small enough that a single failing row doesn't cost
/// thousands of predecessors' worth of redo work on rollback. pnpm's
/// `setRawMany` has no explicit cap (it drains whatever `nextTick`
/// scheduled) but in practice its batches stay in the low hundreds.
const MAX_BATCH_SIZE: usize = 256;

/// Per-query placeholder cap for [`StoreIndex::get_many`]. SQLite's
/// `SQLITE_MAX_VARIABLE_NUMBER` defaulted to 999 before 3.32.0 and is
/// 32766 in newer builds (rusqlite ships a recent SQLite, so the
/// effective cap is well above any realistic lockfile size). Capping
/// at 999 here keeps us safe against hand-rolled custom builds with
/// the legacy default — no realistic install hits this boundary, but
/// the chunking adds maybe a microsecond of overhead and removes the
/// need to think about the cap on the read path.
const GET_MANY_CHUNK: usize = 999;

impl StoreIndexWriter {
    /// Spawn the batched writer task. Returns the handle producers push
    /// rows to, and a [`JoinHandle`][tokio::task::JoinHandle] the caller
    /// must `await` after dropping the last `Arc` to the handle so the
    /// final batch flushes before the install returns.
    ///
    /// The writer task owns the [`StoreIndex`] connection for its entire
    /// lifetime; on DB open failure the task returns the error and the
    /// channel closes on the first producer send.
    pub fn spawn(
        store_dir: &StoreDir,
    ) -> (Arc<StoreIndexWriter>, tokio::task::JoinHandle<Result<(), StoreIndexError>>) {
        let v11_dir = store_dir.v11();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, PackageFilesIndex)>();
        let handle = tokio::task::spawn_blocking(move || -> Result<(), StoreIndexError> {
            let mut index = StoreIndex::open(&v11_dir)?;
            let mut batch: Vec<(String, PackageFilesIndex)> = Vec::with_capacity(MAX_BATCH_SIZE);
            while let Some(first) = rx.blocking_recv() {
                batch.push(first);
                // Drain whatever else is already queued to maximize batch
                // size without ever blocking on the channel — a single
                // `recv` above is the only blocking wait per transaction.
                // Cap at `MAX_BATCH_SIZE` so a producer storm doesn't grow
                // an unbounded buffer on the writer side.
                while batch.len() < MAX_BATCH_SIZE {
                    match rx.try_recv() {
                        Ok(item) => batch.push(item),
                        // `Empty` / `Disconnected` both mean "nothing more
                        // to drain right now" — we flush the current batch
                        // and loop back; if the channel is disconnected
                        // the outer `blocking_recv` returns `None` next
                        // and the task exits cleanly.
                        Err(_) => break,
                    }
                }
                if let Err(error) = index.set_many(batch.drain(..)) {
                    // Drop the batch and keep going. One failed flush
                    // (e.g. a disk-full hiccup) shouldn't silently drop
                    // the rest of the install's entries; the next install
                    // will cache-miss those rows and re-populate them,
                    // matching the "best-effort index" stance the read
                    // path already takes.
                    tracing::warn!(
                        target: "pacquet::store_index",
                        ?error,
                        "batched store-index write failed; dropping this batch and continuing",
                    );
                    batch.clear();
                }
            }
            Ok(())
        });
        (Arc::new(StoreIndexWriter { tx, warn_on_send_failure: AtomicBool::new(true) }), handle)
    }

    /// Queue one `(key, value)` to be flushed in the next transaction.
    ///
    /// Silently drops the entry if the writer task has exited (closed
    /// channel). Matches pnpm's graceful-degradation on failed writes:
    /// the install in flight still completes, the next install misses on
    /// this cache-key and re-downloads. The "channel closed" warning is
    /// logged only on the first failure per writer instance — every
    /// subsequent call would emit the same message, and on a 1352-
    /// snapshot install that's a thousand identical warnings drowning
    /// out real diagnostics.
    pub fn queue(&self, key: String, value: PackageFilesIndex) {
        if let Err(error) = self.tx.send((key, value))
            && self.warn_on_send_failure.swap(false, Ordering::Relaxed)
        {
            tracing::warn!(
                target: "pacquet::store_index",
                ?error,
                "store-index writer channel closed; dropping queued row (further failures silenced)",
            );
        }
    }
}

/// Error type of [`StoreIndex`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum StoreIndexError {
    #[display("Failed to create directory for index.db at {path:?}: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::create_dir))]
    CreateDir {
        path: PathBuf,
        #[error(source)]
        source: std::io::Error,
    },

    #[display("Failed to open index.db at {path:?}: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::open))]
    Open {
        path: PathBuf,
        #[error(source)]
        source: rusqlite::Error,
    },

    #[display("Failed to initialize index.db schema: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::init_schema))]
    InitSchema {
        #[error(source)]
        source: rusqlite::Error,
    },

    #[display("Failed to read from index.db: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::read))]
    Read {
        #[error(source)]
        source: rusqlite::Error,
    },

    #[display("Failed to write to index.db: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::write))]
    Write {
        #[error(source)]
        source: rusqlite::Error,
    },

    #[display("Failed to encode PackageFilesIndex as msgpackr records: {source}")]
    #[diagnostic(transparent)]
    Encode {
        #[error(source)]
        source: crate::msgpackr_records::EncodeError,
    },

    #[display("Failed to decode PackageFilesIndex from msgpack: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::decode))]
    Decode {
        #[error(source)]
        source: rmp_serde::decode::Error,
    },

    #[display("Failed to transcode msgpackr-records payload to plain msgpack: {source}")]
    #[diagnostic(transparent)]
    Transcode {
        #[error(source)]
        source: crate::msgpackr_records::DecodeError,
    },
}

impl StoreIndex {
    /// Open (or create) the `index.db` under `store_dir` and configure the
    /// same PRAGMAs pnpm v11 uses.
    pub fn open(store_dir: &Path) -> Result<Self, StoreIndexError> {
        std::fs::create_dir_all(store_dir).map_err(|source| StoreIndexError::CreateDir {
            path: store_dir.to_path_buf(),
            source,
        })?;
        let db_path = store_dir.join("index.db");
        let conn = Connection::open(&db_path)
            .map_err(|source| StoreIndexError::Open { path: db_path, source })?;

        // Busy-timeout FIRST so the internal busy handler is active during the
        // rest of the setup — on Windows file locking is mandatory and
        // concurrent pacquet / pnpm invocations can contend.
        conn.execute_batch(
            "
            PRAGMA busy_timeout=5000;
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA mmap_size=536870912;
            PRAGMA cache_size=-32000;
            PRAGMA temp_store=MEMORY;
            PRAGMA wal_autocheckpoint=10000;
            CREATE TABLE IF NOT EXISTS package_index (
              key TEXT PRIMARY KEY,
              data BLOB NOT NULL
            ) WITHOUT ROWID;
            ",
        )
        .map_err(|source| StoreIndexError::InitSchema { source })?;

        Ok(StoreIndex { conn })
    }

    /// Open the `index.db` that lives under a [`StoreDir`]'s `v11` subdirectory.
    pub fn open_in(store_dir: &StoreDir) -> Result<Self, StoreIndexError> {
        StoreIndex::open(&store_dir.v11())
    }

    /// Open an existing `index.db` read-only. Skips the schema-mutating
    /// PRAGMAs (`journal_mode=WAL`, `synchronous`, `wal_autocheckpoint`)
    /// and `CREATE TABLE IF NOT EXISTS`, so the call cannot create WAL /
    /// SHM sidecar files or otherwise mutate the store.
    ///
    /// We *do* set `busy_timeout`: it's a connection-local wait, not a
    /// DB mutation, and without it a concurrent writer (pnpm or another
    /// pacquet process) turns every cache lookup during contention into
    /// an immediate `SQLITE_BUSY` — i.e. a spurious cache miss that
    /// triggers a full re-download. 5 s matches the writer side.
    pub fn open_readonly(store_dir: &Path) -> Result<Self, StoreIndexError> {
        let db_path = store_dir.join("index.db");
        let conn = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|source| StoreIndexError::Open { path: db_path.clone(), source })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|source| StoreIndexError::Open { path: db_path, source })?;
        Ok(StoreIndex { conn })
    }

    /// Read-only counterpart to [`StoreIndex::open_in`].
    pub fn open_readonly_in(store_dir: &StoreDir) -> Result<Self, StoreIndexError> {
        StoreIndex::open_readonly(&store_dir.v11())
    }

    /// Open a read-only index wrapped in `Arc<Mutex<…>>` so it can be shared
    /// across the many cache lookups an install performs. Returns `None` if
    /// `index.db` does not yet exist under the store — a first-time install
    /// against an empty store — since there is nothing to read back and every
    /// lookup would be a miss anyway.
    ///
    /// Reusing one connection avoids reopening the SQLite database (and
    /// redoing its PRAGMAs) on every package, which otherwise scales
    /// linearly with the snapshot count.
    pub fn shared_readonly_in(store_dir: &StoreDir) -> Option<SharedReadonlyStoreIndex> {
        let v11_dir = store_dir.v11();
        if !v11_dir.join("index.db").exists() {
            return None;
        }
        StoreIndex::open_readonly(&v11_dir).ok().map(|index| Arc::new(Mutex::new(index)))
    }

    /// Look up a package-files index by key. Returns `Ok(None)` if no row exists.
    ///
    /// Rows come in three flavours and all three decode through one
    /// path:
    /// 1. **pnpm-written**: msgpackr-records, what pnpm's
    ///    `Packr({useRecords: true, …})` emits.
    /// 2. **pacquet-written**: also msgpackr-records, from
    ///    [`encode_package_files_index`][crate::msgpackr_records::encode_package_files_index]
    ///    — pacquet matches pnpm's on-wire shape so the two tools can
    ///    share `index.db`.
    /// 3. **Legacy pacquet-written**: plain MessagePack maps from the
    ///    `rmp_serde::to_vec_named` path used before this PR. These
    ///    may still live in caches that predate the cutover.
    ///
    /// All three route through
    /// [`transcode_to_plain_msgpack`][crate::msgpackr_records::transcode_to_plain_msgpack],
    /// which expands records into plain msgpack maps and narrows the
    /// `float 64` encoding of `checkedAt` back to `uint 64`. Plain
    /// msgpack rows skip the records-expansion (the `records_mode` flag
    /// never flips) but still benefit from the float narrowing. The
    /// result feeds `rmp_serde` to produce a [`PackageFilesIndex`].
    ///
    /// Cost is one `Vec<u8>` allocation + memcpy per read, dwarfed by
    /// the SQLite query and disk I/O.
    pub fn get(&self, key: &str) -> Result<Option<PackageFilesIndex>, StoreIndexError> {
        let row: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT data FROM package_index WHERE key = ?", [key], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(StoreIndexError::Read { source: other }),
            })?;

        let Some(bytes) = row else { return Ok(None) };
        decode_index_value(&bytes).map(Some)
    }

    /// Look up many keys in one trip across the SQLite mutex.
    ///
    /// Returns a `key → PackageFilesIndex` map for every row that exists
    /// and decodes cleanly. Missing keys are simply absent from the map;
    /// rows whose msgpack payload fails to decode are logged at `debug!`
    /// and dropped, matching `load_cached_cas_paths`'s `.ok()?` stance on
    /// the per-key path — a malformed row is treated as a cache miss so
    /// the install falls through to a fresh download.
    ///
    /// SQLite walks the `package_index` PK B-tree once per chunk, so the
    /// per-key query overhead (≈40 µs even for misses) collapses into
    /// one round-trip. With 1352 cache keys against an empty store this
    /// drops the prefetch cost from ~50 ms of N selects to a single
    /// query — see #294 for the cold-cache regression this fixes.
    pub fn get_many(
        &self,
        keys: &[String],
    ) -> Result<HashMap<String, PackageFilesIndex>, StoreIndexError> {
        let mut out = HashMap::with_capacity(keys.len());
        if keys.is_empty() {
            return Ok(out);
        }
        for chunk in keys.chunks(GET_MANY_CHUNK) {
            // Build a `?,?,…?` list whose length matches `chunk`. The
            // only thing interpolated into `sql` is this fixed-shape
            // placeholder string — no caller-supplied bytes ever reach
            // the SQL text. The keys themselves flow through
            // `rusqlite::params_from_iter` below, which routes them via
            // SQLite's prepared-statement parameter binding (the same
            // path every other site in this file uses). Keep the two
            // lines in lock-step: if the placeholder count or the params
            // iterator ever stop matching `chunk.len()`, that's the bug
            // to look at — not SQL injection.
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!("SELECT key, data FROM package_index WHERE key IN ({placeholders})");
            let mut stmt =
                self.conn.prepare(&sql).map_err(|source| StoreIndexError::Read { source })?;
            let params = rusqlite::params_from_iter(chunk.iter().map(String::as_str));
            let rows = stmt
                .query_map(params, |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))
                .map_err(|source| StoreIndexError::Read { source })?;
            for row in rows {
                let (key, bytes) = row.map_err(|source| StoreIndexError::Read { source })?;
                match decode_index_value(&bytes) {
                    Ok(entry) => {
                        out.insert(key, entry);
                    }
                    Err(error) => tracing::debug!(
                        target: "pacquet::store_index",
                        ?key,
                        ?error,
                        "skipping undecodable package_index row in get_many",
                    ),
                }
            }
        }
        Ok(out)
    }

    /// Insert or replace a package-files index.
    ///
    /// Uses the [`encode_package_files_index`][crate::msgpackr_records::encode_package_files_index]
    /// encoder, which emits msgpackr-records bytes that pnpm's
    /// `Packr({useRecords: true, moreTypes: true}).unpack(…)` reads as
    /// the same shape it produces itself. A naive
    /// `rmp_serde::to_vec_named` here produced bytes that pnpm's reader
    /// interpreted as a top-level JS `Map`, making `pkgIndex.files` a
    /// property-access miss and crashing with `files is not iterable`
    /// inside pnpm's CAFS layer.
    pub fn set(&self, key: &str, value: &PackageFilesIndex) -> Result<(), StoreIndexError> {
        let buf = crate::msgpackr_records::encode_package_files_index(value)
            .map_err(|source| StoreIndexError::Encode { source })?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO package_index (key, data) VALUES (?1, ?2)",
                rusqlite::params![key, buf],
            )
            .map(|_| ())
            .map_err(|source| StoreIndexError::Write { source })
    }

    /// Insert-or-replace a batch of rows in a single transaction.
    ///
    /// Matches pnpm's `setRawMany` (see `store/index/src/index.ts`): one
    /// `BEGIN IMMEDIATE` … `COMMIT` around the inserts, which amortizes the
    /// WAL commit fsync across the whole batch. At 1352 snapshots that
    /// turns 1352 per-row fsyncs into ⌈1352/batch_size⌉ — on APFS this is
    /// the single biggest lever for `pacquet install` wall time (#263).
    ///
    /// SQLite errors during the transaction roll it back before returning,
    /// so a partial apply never leaves the index in a half-written state.
    /// A per-row msgpack encoding error is logged at `warn!` and skipped
    /// — one malformed `PackageFilesIndex` shouldn't cost every other row
    /// in the batch the chance to commit, matching the "best-effort
    /// index" stance the writer task and the read path already take.
    /// Encoding is done up front into a `Vec<(String, Vec<u8>)>` so the
    /// transaction body is pure SQLite — the caller's producer thread pays
    /// the msgpack cost, not the single writer thread.
    pub fn set_many(
        &mut self,
        entries: impl IntoIterator<Item = (String, PackageFilesIndex)>,
    ) -> Result<(), StoreIndexError> {
        // Encode outside the transaction so a single malformed row can't
        // hold `BEGIN IMMEDIATE`'s write lock while we serialize msgpack,
        // and skip individual encoding failures with a log so one bad
        // entry doesn't drop the rest of the batch on the floor.
        let mut encoded: Vec<(String, Vec<u8>)> = Vec::new();
        for (key, value) in entries {
            match crate::msgpackr_records::encode_package_files_index(&value) {
                Ok(buf) => encoded.push((key, buf)),
                Err(source) => tracing::warn!(
                    target: "pacquet::store_index",
                    ?key,
                    error = ?source,
                    "failed to encode package_index row; skipping",
                ),
            }
        }
        if encoded.is_empty() {
            return Ok(());
        }

        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|source| StoreIndexError::Write { source })?;
        {
            let mut stmt = tx
                .prepare_cached("INSERT OR REPLACE INTO package_index (key, data) VALUES (?1, ?2)")
                .map_err(|source| StoreIndexError::Write { source })?;
            for (key, buf) in &encoded {
                stmt.execute(rusqlite::params![key, buf])
                    .map_err(|source| StoreIndexError::Write { source })?;
            }
        }
        tx.commit().map_err(|source| StoreIndexError::Write { source })
    }

    /// `true` iff a row with this key exists.
    pub fn contains_key(&self, key: &str) -> Result<bool, StoreIndexError> {
        let exists = self
            .conn
            .query_row("SELECT 1 FROM package_index WHERE key = ?", [key], |_| Ok(()))
            .map(|_| true)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(StoreIndexError::Read { source: other }),
            })?;
        Ok(exists)
    }

    /// Collect every key in `package_index`. Useful for tests and store-prune.
    /// Buffers to avoid holding a statement borrow across the returned vector.
    pub fn keys(&self) -> Result<Vec<String>, StoreIndexError> {
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM package_index")
            .map_err(|source| StoreIndexError::Read { source })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| StoreIndexError::Read { source })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|source| StoreIndexError::Read { source })?);
        }
        Ok(out)
    }
}

fn decode_index_value(bytes: &[u8]) -> Result<PackageFilesIndex, StoreIndexError> {
    // `transcode_to_plain_msgpack` tracks records-mode internally and
    // only reinterprets `0x40..=0x7f` as slot references after a record
    // definition has been observed, so it's safe to run on both
    // pacquet-written (plain msgpack) and pnpm-written (msgpackr records)
    // rows. For plain rows it still performs the integer-valued float
    // narrowing we need on the read side — pacquet writes the
    // `checkedAt` timestamp as `float 64` for JS/BigInt interop.
    let plain = crate::msgpackr_records::transcode_to_plain_msgpack(bytes)
        .map_err(|source| StoreIndexError::Transcode { source })?;
    rmp_serde::from_slice(&plain).map_err(|source| StoreIndexError::Decode { source })
}

/// Build the SQLite key pnpm uses: `"{integrity}\t{pkg_id}"`. Integrity strings
/// never contain tabs so the separator is unambiguous.
pub fn store_index_key(integrity: &str, pkg_id: &str) -> String {
    format!("{integrity}\t{pkg_id}")
}

/// Per-instance record of what a tarball contributed to the CAFS. Stored as the
/// value half of each `package_index` row.
///
/// Mirrors pnpm v11's `PackageFilesIndex` from `store/cafs/src/checkPkgFilesIntegrity.ts`.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFilesIndex {
    /// Subset of the tarball's `package.json` that pnpm keeps on hand to avoid
    /// re-reading the manifest for each install. Pacquet currently writes this
    /// as `None`; fill in later when we start populating build metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<serde_json::Value>,

    /// Whether the package's lifecycle scripts demand a build step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_build: Option<bool>,

    /// The digest algorithm used for every `files.*.digest` entry, e.g. `sha512`.
    pub algo: String,

    /// Map of in-tarball path → CAFS file metadata.
    pub files: HashMap<String, CafsFileInfo>,

    /// Side-effect overlays applied after post-install scripts. Populated by
    /// the build-side-effects cache; pacquet does not yet write this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<HashMap<String, SideEffectsDiff>>,
}

/// Value of [`PackageFilesIndex::files`]. Mirrors pnpm v11's
/// [`PackageFileInfo`](https://github.com/pnpm/pnpm/blob/1819226b51/store/cafs-types/src/index.ts)
/// field-for-field so that the msgpack payload interops.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CafsFileInfo {
    /// Content-addressed digest of the file — raw hex (no `sha512-` prefix),
    /// matching pnpm v11's `digest` field in the cafs index.
    pub digest: String,
    pub mode: u32,
    pub size: u64,
    /// Millisecond Unix timestamp of the last integrity check, or `None`
    /// if never verified.
    ///
    /// Wire note: serialized as MessagePack `float 64` so the byte
    /// encoding matches what pnpm itself emits (JS `Number` is a double,
    /// so msgpackr writes timestamps past int32 range as `cb` + 8
    /// bytes). Writing as `uint 64` instead would be "correct" MessagePack
    /// but msgpackr would decode it as a `BigInt`, and pnpm's integrity
    /// check does `mtimeMs - (checkedAt ?? 0)` — mixing Number and
    /// BigInt throws `TypeError` at runtime. On the read side, the
    /// [`transcode_to_plain_msgpack`][crate::msgpackr_records::transcode_to_plain_msgpack]
    /// step narrows integer-valued floats back to `uint 64` so
    /// `rmp_serde` can deserialize into `Option<u64>` without complaint.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_checked_at")]
    pub checked_at: Option<u64>,
}

/// Emit `Option<u64>` on the msgpack wire as `float 64` rather than
/// `uint 64`. See the doc on [`CafsFileInfo::checked_at`] for the
/// interop reasoning — short version, msgpackr reads `uint 64` as a
/// `BigInt` and pnpm's integrity check then crashes on Number/BigInt
/// mixing.
fn serialize_checked_at<S: serde::Serializer>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(v) => serializer.serialize_f64(*v as f64),
        None => serializer.serialize_none(),
    }
}

/// Value of [`PackageFilesIndex::side_effects`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideEffectsDiff {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<HashMap<String, CafsFileInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<Vec<String>>,
}

#[cfg(test)]
mod tests;
