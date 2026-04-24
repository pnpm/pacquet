use dashmap::DashSet;
use serde::{Deserialize, Serialize};
use sha2::{digest, Sha512};
use std::path::{self, PathBuf};

/// Content hash of a file.
pub type FileHash = digest::Output<Sha512>;

/// Represent a store directory.
///
/// * The store directory stores all files that were acquired by installing packages with pacquet or pnpm.
/// * The files in `node_modules` directories are hardlinks or reflinks to the files in the store directory.
/// * The store directory can and often act as a global shared cache of all installation of different workspaces.
/// * The location of the store directory can be customized by `store-dir` field.
/// * The on-disk layout matches pnpm v11 (`<root>/v11/files/XX/…[-exec]` + `<root>/v11/index.db`)
///   so the two tools can share a store.
#[derive(Debug, Deserialize, Serialize)]
#[serde(transparent)]
pub struct StoreDir {
    /// Path to the root of the store directory from which all sub-paths are derived.
    ///
    /// Consumer of this struct should interact with the sub-paths instead of this path.
    root: PathBuf,

    /// Runtime cache of shard bytes (`files/XX/`) this process has already
    /// ensured exist. The CAS layout has exactly 256 shards keyed by the
    /// first byte of the sha512 digest; `create_dir_all` is idempotent but
    /// does a `stat` syscall every call even when the directory already
    /// exists, and a cold install of ~10k files would otherwise pay that
    /// `stat` per file. After the first hit, the shard is cached and
    /// subsequent writes skip the syscall entirely. Populated lazily by
    /// [`StoreDir::write_cas_file`]; duplicate inserts across threads are
    /// harmless since `create_dir_all` is idempotent.
    #[serde(skip, default)]
    ensured_shards: DashSet<u8>,
}

/// Manual `PartialEq` / `Eq`: the shard cache is runtime state, two stores
/// are equal iff they point at the same path.
impl PartialEq for StoreDir {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
    }
}

impl Eq for StoreDir {}

impl From<PathBuf> for StoreDir {
    fn from(root: PathBuf) -> Self {
        StoreDir { root, ensured_shards: DashSet::new() }
    }
}

impl StoreDir {
    /// Construct an instance of [`StoreDir`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        root.into().into()
    }

    /// Mark the shard keyed by the first byte of a sha512 digest as "parent
    /// directory already created this process". Used by
    /// [`StoreDir::write_cas_file`] to skip `create_dir_all` on subsequent
    /// writes into the same shard.
    pub(crate) fn mark_shard_ensured(&self, shard_byte: u8) {
        self.ensured_shards.insert(shard_byte);
    }

    /// Fast-path check: did this process already ensure the shard dir for
    /// this byte exists? Returns `true` once, per shard, per process.
    pub(crate) fn shard_already_ensured(&self, shard_byte: u8) -> bool {
        self.ensured_shards.contains(&shard_byte)
    }

    /// Create an object that [displays](std::fmt::Display) the root of the store directory.
    pub fn display(&self) -> path::Display<'_> {
        self.root.display()
    }

    /// Get `{store}/v11` — the root of the pnpm v11 store layout.
    pub fn v11(&self) -> PathBuf {
        self.root.join("v11")
    }

    /// The directory that contains all content-addressed files.
    fn files(&self) -> PathBuf {
        self.v11().join("files")
    }

    /// Path to a file in the store directory.
    ///
    /// **Parameters:**
    /// * `head` is the first 2 hexadecimal digit of the file address.
    /// * `tail` is the rest of the address and an optional suffix.
    fn file_path_by_head_tail(&self, head: &str, tail: &str) -> PathBuf {
        self.files().join(head).join(tail)
    }

    /// Path to a content-addressed file. The hex digest is split into a
    /// two-char prefix directory and the remainder, plus an optional `-exec`
    /// suffix for executable files — this is pnpm v11's `files/XX/<rest>[-exec]`
    /// layout.
    pub(crate) fn file_path_by_hex_str(&self, hex: &str, suffix: &'static str) -> PathBuf {
        let head = &hex[..2];
        let middle = &hex[2..];
        let tail = format!("{middle}{suffix}");
        self.file_path_by_head_tail(head, &tail)
    }

    /// Path to the temporary directory inside the store.
    pub fn tmp(&self) -> PathBuf {
        self.v11().join("tmp")
    }

    /// Eagerly create `<store>/v11/files/` plus every `files/XX/` shard
    /// (00..ff). Ports pnpm's
    /// [`initStore`](https://github.com/pnpm/pnpm/blob/main/worker/src/start.ts)
    /// worker routine and its gating check in
    /// [`createPackageStore`](https://github.com/pnpm/pnpm/blob/main/store/package-store/src/storeController/index.ts):
    /// when `files/` doesn't exist yet, we create all 256 shards up front
    /// so CAFS writes never pay a `create_dir_all` syscall in the hot
    /// path. When `files/` already exists we assume the layout is intact
    /// and just seed the shard cache so the write-side fast path
    /// applies immediately.
    ///
    /// Errors from individual shard mkdirs are ignored when the error is
    /// [`AlreadyExists`][std::io::ErrorKind::AlreadyExists] — matching
    /// pnpm's try/catch per shard, which treats a parallel process
    /// racing the same layout as benign. Other errors propagate; the
    /// caller degrades them to a warning and falls back to the per-
    /// write lazy mkdir in [`StoreDir::write_cas_file`].
    pub fn init(&self) -> std::io::Result<()> {
        let files = self.files();
        let already_exists = files.exists();
        std::fs::create_dir_all(&files)?;
        for shard in 0u8..=255 {
            // Two-char lowercase hex keyed off the first byte of the
            // sha512 digest, matching `StoreDir::file_path_by_hex_str`.
            let shard_dir = files.join(format!("{shard:02x}"));
            if !already_exists {
                if let Err(error) = std::fs::create_dir(&shard_dir) {
                    if error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
            }
            self.mark_shard_ensured(shard);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;

    #[test]
    fn file_path_by_head_tail() {
        let received = "/home/user/.local/share/pnpm/store"
            .pipe(StoreDir::new)
            .file_path_by_head_tail("3e", "f722d37b016c63ac0126cfdcec");
        let expected = PathBuf::from(
            "/home/user/.local/share/pnpm/store/v11/files/3e/f722d37b016c63ac0126cfdcec",
        );
        assert_eq!(&received, &expected);
    }

    #[test]
    fn tmp() {
        let received = StoreDir::new("/home/user/.local/share/pnpm/store").tmp();
        let expected = PathBuf::from("/home/user/.local/share/pnpm/store/v11/tmp");
        assert_eq!(&received, &expected);
    }

    /// `init` on a fresh store should materialize `v11/files/00..ff`
    /// and populate the shard cache so later `write_cas_file` calls
    /// can skip their lazy mkdir.
    #[test]
    fn init_creates_all_256_shards_and_populates_cache() {
        use tempfile::tempdir;

        let tempdir = tempdir().unwrap();
        let store = StoreDir::new(tempdir.path());
        store.init().unwrap();

        let files = tempdir.path().join("v11/files");
        assert!(files.is_dir(), "v11/files must exist after init");
        for shard in 0u8..=255 {
            let name = format!("{shard:02x}");
            assert!(files.join(&name).is_dir(), "shard {name} must exist after init");
            assert!(
                store.shard_already_ensured(shard),
                "shard {name} must be marked ensured in the cache"
            );
        }
    }

    /// `init` on a store where `files/` already exists should skip the
    /// per-shard mkdir work and just seed the cache — pnpm's gating
    /// `existsSync` check. A later out-of-band removal of a shard
    /// would still resolve at write time via the lazy fallback in
    /// `write_cas_file`, so we don't verify every shard dir here —
    /// only that init doesn't re-create anything it doesn't have to
    /// and that the cache is primed.
    #[test]
    fn init_warm_store_seeds_cache_without_recreating_shards() {
        use tempfile::tempdir;

        let tempdir = tempdir().unwrap();
        let files = tempdir.path().join("v11/files");
        std::fs::create_dir_all(&files).unwrap();
        // Create a sentinel inside a shard so we can prove init didn't
        // wipe or race-recreate it; plain `mkdir` of an existing dir
        // would fail anyway (EEXIST), but an aggressive port could
        // accidentally `remove_dir_all` + recreate, so pin the
        // invariant.
        let shard = files.join("00");
        std::fs::create_dir(&shard).unwrap();
        std::fs::write(shard.join("sentinel"), b"do not delete me").unwrap();

        let store = StoreDir::new(tempdir.path());
        store.init().unwrap();

        assert!(shard.join("sentinel").is_file(), "pre-existing shard content must survive init");
        for shard in 0u8..=255 {
            assert!(
                store.shard_already_ensured(shard),
                "shard {shard:02x} must be marked ensured even on warm store"
            );
        }
    }
}
