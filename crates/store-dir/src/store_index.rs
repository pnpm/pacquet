use crate::StoreDir;
use derive_more::{Display, Error};
use miette::Diagnostic;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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
/// [`store/index/src/index.ts`](https://github.com/pnpm/pnpm/blob/main/store/index/src/index.ts)
/// so that the two tools can read each other's entries.
pub struct StoreIndex {
    conn: Connection,
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

    #[display("Failed to encode PackageFilesIndex as msgpack: {source}")]
    #[diagnostic(code(pacquet_store_dir::store_index::encode))]
    Encode {
        #[error(source)]
        source: rmp_serde::encode::Error,
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

    /// Look up a package-files index by key. Returns `Ok(None)` if no row exists.
    ///
    /// pacquet-written rows are plain `rmp_serde` msgpack maps (via
    /// `to_vec_named`). pnpm-written rows use msgpackr's records extension
    /// — a shape we route through [`transcode_to_plain_msgpack`][crate::msgpackr_records::transcode_to_plain_msgpack]
    /// so `rmp_serde` never has to know about slot bytes or ext type 0x72.
    /// We sniff the leading two bytes (`d4 72` == fixext1 + records ext
    /// type) to avoid transcoding pacquet's own rows, which don't need
    /// it and would otherwise pay an extra allocation per read.
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

    /// Insert or replace a package-files index.
    pub fn set(&self, key: &str, value: &PackageFilesIndex) -> Result<(), StoreIndexError> {
        // `to_vec_named` writes structs as string-keyed maps rather than
        // positional arrays — required for pnpm-interop, since pnpm's
        // msgpackr reads/writes named-field records.
        let buf =
            rmp_serde::to_vec_named(value).map_err(|source| StoreIndexError::Encode { source })?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO package_index (key, data) VALUES (?1, ?2)",
                rusqlite::params![key, buf],
            )
            .map(|_| ())
            .map_err(|source| StoreIndexError::Write { source })
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

/// First two bytes emitted by msgpackr when `useRecords: true` is on —
/// fixext1 header + the 0x72 ("r") record-definition ext type. Every
/// pnpm-written `PackageFilesIndex` row opens with this pair because
/// the top-level struct itself is a record.
const MSGPACKR_RECORDS_MARKER: [u8; 2] = [0xd4, crate::msgpackr_records::RECORD_DEF_EXT_TYPE];

fn decode_index_value(bytes: &[u8]) -> Result<PackageFilesIndex, StoreIndexError> {
    if bytes.starts_with(&MSGPACKR_RECORDS_MARKER) {
        let plain = crate::msgpackr_records::transcode_to_plain_msgpack(bytes)
            .map_err(|source| StoreIndexError::Transcode { source })?;
        rmp_serde::from_slice(&plain).map_err(|source| StoreIndexError::Decode { source })
    } else {
        rmp_serde::from_slice(bytes).map_err(|source| StoreIndexError::Decode { source })
    }
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
/// [`PackageFileInfo`](https://github.com/pnpm/pnpm/blob/main/store/cafs-types/src/index.ts)
/// field-for-field so that the msgpack payload interops.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CafsFileInfo {
    /// Content-addressed digest of the file — raw hex (no `sha512-` prefix),
    /// matching pnpm v11's `digest` field in the cafs index.
    pub digest: String,
    pub mode: u32,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<u128>,
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
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn sample_index() -> PackageFilesIndex {
        let mut files = HashMap::new();
        files.insert(
            "package.json".to_string(),
            CafsFileInfo {
                checked_at: Some(1_700_000_000_000),
                digest: "abc".to_string(),
                mode: 0o644,
                size: 123,
            },
        );
        files.insert(
            "index.js".to_string(),
            CafsFileInfo { checked_at: None, digest: "def".to_string(), mode: 0o755, size: 42 },
        );
        PackageFilesIndex {
            manifest: None,
            requires_build: Some(false),
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        }
    }

    #[test]
    fn key_format_is_integrity_tab_pkg_id() {
        assert_eq!(store_index_key("sha512-abc", "lodash@4.17.21"), "sha512-abc\tlodash@4.17.21");
    }

    #[test]
    fn set_then_get_round_trips() {
        let dir = tempdir().unwrap();
        let idx = StoreIndex::open(dir.path()).unwrap();
        let key = store_index_key("sha512-xyz", "pkg@1.0.0");
        let original = sample_index();

        idx.set(&key, &original).unwrap();
        let loaded = idx.get(&key).unwrap().expect("row must exist after set");

        assert_eq!(loaded, original);
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let dir = tempdir().unwrap();
        let idx = StoreIndex::open(dir.path()).unwrap();
        assert!(idx.get("sha512-never\tnone@0.0.0").unwrap().is_none());
        assert!(!idx.contains_key("sha512-never\tnone@0.0.0").unwrap());
    }

    #[test]
    fn set_is_upsert() {
        let dir = tempdir().unwrap();
        let idx = StoreIndex::open(dir.path()).unwrap();
        let key = store_index_key("sha512-abc", "pkg@1.0.0");

        let first = sample_index();
        idx.set(&key, &first).unwrap();

        let mut second = sample_index();
        second.algo = "sha256".to_string();
        idx.set(&key, &second).unwrap();

        let loaded = idx.get(&key).unwrap().unwrap();
        assert_eq!(loaded.algo, "sha256");
    }

    #[test]
    fn reopening_the_same_db_sees_prior_writes() {
        let dir = tempdir().unwrap();
        let key = store_index_key("sha512-abc", "pkg@1.0.0");
        let payload = sample_index();

        {
            let idx = StoreIndex::open(dir.path()).unwrap();
            idx.set(&key, &payload).unwrap();
        }

        let idx = StoreIndex::open(dir.path()).unwrap();
        assert_eq!(idx.get(&key).unwrap().unwrap(), payload);
    }

    #[test]
    fn index_db_lives_at_store_dir_v11() {
        let root = tempdir().unwrap();
        let store = StoreDir::new(root.path());
        let idx = StoreIndex::open_in(&store).unwrap();
        idx.set("k\tv", &sample_index()).unwrap();
        assert!(store.v11().join("index.db").exists());
    }

    /// A row whose bytes are msgpackr-records (as pnpm writes) must decode
    /// through `StoreIndex::get` just like a pacquet-written row. The
    /// fixture here is the same "one-file index" bytes used in the
    /// `msgpackr_records` unit tests — inserted via a direct SQL write so
    /// we test the decoder *through the get path*, not the round-trip.
    #[test]
    fn get_decodes_msgpackr_records_rows() {
        let dir = tempdir().unwrap();
        let idx = StoreIndex::open(dir.path()).unwrap();
        let key = "sha512-xyz\tfake@1.0.0";

        // Captured from `node /tmp/msgpackr_fixture.mjs`, "one-file index".
        let msgpackr_row: &[u8] = &[
            0xd4, 0x72, 0x40, 0x92, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xa6, 0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0x81, 0xac, 0x70, 0x61, 0x63, 0x6b,
            0x61, 0x67, 0x65, 0x2e, 0x6a, 0x73, 0x6f, 0x6e, 0xd4, 0x72, 0x41, 0x94, 0xa6, 0x64,
            0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d, 0x6f, 0x64, 0x65, 0xa4, 0x73, 0x69, 0x7a,
            0x65, 0xa9, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x65, 0x64, 0x41, 0x74, 0xa3, 0x61, 0x62,
            0x63, 0xcd, 0x01, 0xa4, 0x11, 0xcb, 0x42, 0x78, 0xbc, 0xfe, 0x56, 0x80, 0x00, 0x00,
        ];
        idx.conn
            .execute(
                "INSERT INTO package_index (key, data) VALUES (?1, ?2)",
                rusqlite::params![key, msgpackr_row],
            )
            .unwrap();

        let loaded = idx.get(key).unwrap().expect("row must decode");
        assert_eq!(loaded.algo, "sha512");
        let info = loaded.files.get("package.json").unwrap();
        assert_eq!(info.digest, "abc");
        assert_eq!(info.mode, 0o644);
        assert_eq!(info.size, 17);
        assert_eq!(info.checked_at, Some(1_700_000_000_000));
    }
}
