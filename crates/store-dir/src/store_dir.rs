use dashmap::DashSet;
use serde::{Deserialize, Serialize};
use sha2::{Sha512, digest};
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

    /// On a fresh store, eagerly create `<store>/v11/files/` plus every
    /// `files/XX/` shard (00..ff) and seed the shard cache with the
    /// bytes we just created, so CAFS writes never pay a
    /// `create_dir_all` syscall in the hot path.
    ///
    /// Gated by an `is_dir()` check on `files/` so we only run when the
    /// store is truly fresh — spiritually matches pnpm's
    /// [`createPackageStore`](https://github.com/pnpm/pnpm/blob/1819226b51/store/controller/src/storeController/index.ts)
    /// guard (`if !fs.existsSync(path.join(storeDir, 'files')) initStoreDir(...)`),
    /// but tightened from `exists()` to `is_dir()` so a non-directory
    /// entry at `files/` doesn't let `init` silently noop past store
    /// corruption. On a warm store this is a single stat and we
    /// return `Ok(())` without seeding the cache: a store created by
    /// an older pacquet that only lazily materialized shards might
    /// not have every `files/XX/` on disk, and pre-seeding the cache
    /// would let a later `write_cas_file` skip `ensure_parent_dir`
    /// and then fail at `open` with `NotFound`. Leaving the cache
    /// empty on warm store lets the lazy mkdir fallback inside
    /// [`StoreDir::write_cas_file`] populate it per shard on first
    /// write — the same shape pnpm uses via `writeFile.ts`'s `dirs`
    /// Set.
    ///
    /// Errors from individual shard mkdirs are ignored when the error is
    /// [`AlreadyExists`][std::io::ErrorKind::AlreadyExists] **and** the
    /// existing entry is actually a directory (via
    /// [`Path::is_dir`][std::path::Path::is_dir], which follows
    /// symlinks — a symlink pointing at a real directory
    /// is accepted, matching what ops folks sometimes do to spread a
    /// store across disks). This matches pnpm's try/catch per shard
    /// (parallel process racing the same layout is benign) but
    /// tightens it slightly: a regular file, a non-directory symlink,
    /// or a broken symlink squatting on the shard path is rejected
    /// instead of being cached as ensured. Other errors propagate; the
    /// caller degrades them to a warning and falls back to the per-
    /// write lazy mkdir.
    pub fn init(&self) -> std::io::Result<()> {
        let files = self.files();
        // `is_dir()` rather than `exists()`: if `files` is present but
        // isn't a directory (regular file, broken symlink, other
        // corruption), a permissive `exists()` check would make `init`
        // a silent noop and later `write_cas_file` calls would fail
        // with cryptic per-file `open` errors. Gating on `is_dir()`
        // lets the `create_dir_all` below surface a clear "not a
        // directory" error from the kernel, which the caller degrades
        // to a `warn!` at install bootstrap.
        if files.is_dir() {
            return Ok(());
        }
        std::fs::create_dir_all(&files)?;
        for shard in 0u8..=255 {
            // Two-char lowercase hex keyed off the first byte of the
            // sha512 digest, matching `StoreDir::file_path_by_hex_str`.
            let shard_dir = files.join(format!("{shard:02x}"));
            if let Err(error) = std::fs::create_dir(&shard_dir) {
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
                // `AlreadyExists` is benign only when the existing
                // entry resolves to a directory — a parallel pnpm
                // or pacquet process racing the same layout is
                // fine, and a symlink pointing at a real directory
                // is too (ops folks occasionally spread a store
                // across disks that way). `Path::is_dir` follows
                // symlinks, which is the desired semantics here. A
                // regular file, a non-dir symlink, or a broken
                // symlink would make `mark_shard_ensured` a lie and
                // punt the failure to a much less actionable
                // `open` error inside the per-file CAFS write.
                // Reject upfront.
                if !shard_dir.is_dir() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!(
                            "CAFS shard path {} exists but does not resolve to a directory",
                            shard_dir.display(),
                        ),
                    ));
                }
            }
            self.mark_shard_ensured(shard);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::StoreDir;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[test]
    fn file_path_by_head_tail() {
        let received = "/home/user/.local/share/pnpm/store"
            .pipe(StoreDir::new)
            .file_path_by_head_tail("3e", "f722d37b016c63ac0126cfdcec");
        let expected =
            Path::new("/home/user/.local/share/pnpm/store/v11/files/3e/f722d37b016c63ac0126cfdcec");
        assert_eq!(received, expected);
    }

    #[test]
    fn tmp() {
        let received = StoreDir::new("/home/user/.local/share/pnpm/store").tmp();
        let expected = Path::new("/home/user/.local/share/pnpm/store/v11/tmp");
        assert_eq!(received, expected);
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

    /// `init` on a store where `files/` already exists must be a
    /// near-noop: don't re-create anything, don't seed the cache. A
    /// store created by an older pacquet might be missing shard dirs
    /// we never materialized, and pre-seeding the cache in that case
    /// would let `write_cas_file` skip `ensure_parent_dir` and blow up
    /// at `open`. Leaving the cache empty keeps the lazy fallback in
    /// `write_cas_file` responsible for materializing each shard the
    /// first time it's written, matching pnpm's `writeFile.ts` `dirs`
    /// Set.
    /// If `v11/files/` is present but isn't a directory (store
    /// corruption — a regular file landed there somehow), `init` must
    /// surface a clear `io::Error` rather than silently becoming a noop
    /// and letting each later `write_cas_file` fail with a less
    /// actionable per-file `open` error. `create_dir_all` on a path
    /// where a component is already a regular file returns an error
    /// from the OS; we just need the gate to be tight enough to let it
    /// run.
    #[test]
    fn init_rejects_non_directory_files_path() {
        use tempfile::tempdir;

        let tempdir = tempdir().unwrap();
        let v11 = tempdir.path().join("v11");
        std::fs::create_dir_all(&v11).unwrap();
        std::fs::write(v11.join("files"), b"i am not a directory").unwrap();

        let store = StoreDir::new(tempdir.path());
        // Don't pin the exact ErrorKind — platforms differ
        // (`NotADirectory` on Linux, `AlreadyExists` / `Uncategorized`
        // elsewhere). `expect_err` asserting that *an* error surfaced
        // is enough; the caller has already wired it through `warn!`.
        store.init().expect_err("init must fail when files/ isn't a directory");
        for shard in 0u8..=255 {
            assert!(
                !store.shard_already_ensured(shard),
                "a failing init must not seed the shard cache"
            );
        }
    }

    #[test]
    fn init_warm_store_is_noop_and_leaves_cache_empty() {
        use tempfile::tempdir;

        let tempdir = tempdir().unwrap();
        let files = tempdir.path().join("v11/files");
        std::fs::create_dir_all(&files).unwrap();
        // Plant a sentinel inside a pre-existing shard so we can prove
        // init didn't wipe or re-create it. Plain `mkdir` of an existing
        // dir would fail anyway (EEXIST), but an aggressive port could
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
                !store.shard_already_ensured(shard),
                "shard {shard:02x} must NOT be marked ensured on warm-store init — the cache is populated lazily from write_cas_file"
            );
        }
    }
}
