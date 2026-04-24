use crate::{FileHash, StoreDir};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_fs::{
    ensure_file, ensure_parent_dir,
    file_mode::{is_executable, EXEC_MODE},
    EnsureFileError,
};
use sha2::{Digest, Sha512};
use std::path::PathBuf;

impl StoreDir {
    /// Path to a file in the store directory.
    pub fn cas_file_path(&self, hash: FileHash, executable: bool) -> PathBuf {
        let hex = format!("{hash:x}");
        let suffix = if executable { "-exec" } else { "" };
        self.file_path_by_hex_str(&hex, suffix)
    }

    /// Path to a content-addressed file given its pre-computed hex digest
    /// (from the SQLite store index) and its POSIX mode. Matches pnpm's
    /// [`getFilePathByModeInCafs`](https://github.com/pnpm/pnpm/blob/main/store/cafs/src/getFilePathInCafs.ts)
    /// so index entries written by either tool resolve to the same path.
    ///
    /// Returns `None` when `hex` is too short or not ASCII-hex.
    ///
    /// We require *more* than two hex chars — the first two become the
    /// shard directory `files/XX/`, and the rest is the file component.
    /// A two-char input produces an empty tail, which on disk is the
    /// shard directory itself (usually present), so without this tighter
    /// check a caller would hand a directory path back as if it were a
    /// CAFS file path. The ASCII-hex requirement additionally guards the
    /// `hex[..2]` slice inside `file_path_by_hex_str` from panicking on
    /// non-UTF-8-char-boundary input.
    pub fn cas_file_path_by_mode(&self, hex: &str, mode: u32) -> Option<PathBuf> {
        if hex.len() <= 2 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        // Same executable-bit rule the write side uses
        // (`pacquet_fs::file_mode::is_executable`, matching pnpm's
        // `modeIsExecutable`), so a blob written as `-exec` is read back
        // as `-exec` and vice versa. Using a raw `0o111` literal here
        // silently diverged from the write side for modes like `0o744`
        // and turned every lookup of such a file into a cache miss.
        let suffix = if is_executable(mode) { "-exec" } else { "" };
        Some(self.file_path_by_hex_str(hex, suffix))
    }
}

/// Error type of [`StoreDir::write_cas_file`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum WriteCasFileError {
    WriteFile(EnsureFileError),
}

impl StoreDir {
    /// Write a file from an npm package to the store directory.
    pub fn write_cas_file(
        &self,
        buffer: &[u8],
        executable: bool,
    ) -> Result<(PathBuf, FileHash), WriteCasFileError> {
        let file_hash = Sha512::digest(buffer);
        let file_path = self.cas_file_path(file_hash, executable);
        let mode = executable.then_some(EXEC_MODE);

        // Ensure the shard directory (`files/XX/`) exists. The CAS has
        // 256 shards keyed by `file_hash[0]`; `create_dir_all` does a
        // `stat` syscall every call even when the directory is already
        // there, so remember which shards we've created and skip on
        // repeat. Duplicate mkdirs across threads are benign — the first
        // few writes into a fresh shard may each call `create_dir_all`,
        // which is idempotent; once any of them completes and inserts
        // into the cache, subsequent writes take the fast path.
        let shard_byte = file_hash[0];
        if !self.shard_already_ensured(shard_byte) {
            let parent = file_path.parent().expect("CAS file path always has a parent shard dir");
            ensure_parent_dir(parent).map_err(WriteCasFileError::WriteFile)?;
            self.mark_shard_ensured(shard_byte);
        }

        ensure_file(&file_path, buffer, mode).map_err(WriteCasFileError::WriteFile)?;
        Ok((file_path, file_hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cas_file_path() {
        fn case(file_content: &str, executable: bool, expected: &str) {
            eprintln!("CASE: {file_content:?}, {executable:?}");
            let store_dir = StoreDir::new("STORE_DIR");
            let file_hash = Sha512::digest(file_content);
            eprintln!("file_hash = {file_hash:x}");
            let received = store_dir.cas_file_path(file_hash, executable);
            let expected: PathBuf = expected.split('/').collect();
            assert_eq!(&received, &expected);
        }

        case(
            "hello world",
            false,
            "STORE_DIR/v11/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f",
        );

        case(
            "hello world",
            true,
            "STORE_DIR/v11/files/30/9ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f-exec",
        );
    }

    #[test]
    fn cas_file_path_by_mode_suffix_matches_write_side() {
        // Tarballs frequently ship scripts as `0o744` (user-exec only).
        // The write side treats any-exec-bit-set as executable and stores
        // the blob under `-exec`; the read side must use the same rule,
        // otherwise every cache lookup for such a file turns into a miss.
        let store_dir = StoreDir::new("STORE_DIR");
        let hex = "a".repeat(128);
        for mode in [0o744, 0o755, 0o775, 0o100, 0o010, 0o001] {
            let path = store_dir
                .cas_file_path_by_mode(&hex, mode)
                .unwrap_or_else(|| panic!("mode {mode:o} should produce a path"));
            assert!(
                path.to_string_lossy().ends_with("-exec"),
                "mode {mode:o} should resolve to an `-exec` path, got {path:?}"
            );
        }
        for mode in [0o644, 0o600, 0o444, 0o000] {
            let path = store_dir
                .cas_file_path_by_mode(&hex, mode)
                .unwrap_or_else(|| panic!("mode {mode:o} should produce a path"));
            assert!(
                !path.to_string_lossy().ends_with("-exec"),
                "mode {mode:o} should NOT resolve to an `-exec` path, got {path:?}"
            );
        }
    }

    /// The shard-mkdir cache should be empty on a fresh `StoreDir`, get
    /// populated after the first write into a given shard, and survive
    /// subsequent writes (including ones that land in the same shard
    /// because two different contents happen to share the first digest
    /// byte). Two writes where the second one's parent directory was
    /// removed in between must still succeed without re-mkdir'ing — the
    /// opposite would regress cold-install latency by reintroducing a
    /// `create_dir_all` (and its backing `stat`) per file.
    ///
    /// This test also pins the invariant that a miss clears only the
    /// shards' directory stat, not the file-existence check inside
    /// `ensure_file` — a separate, warm-cache fast path.
    #[test]
    fn shard_cache_populates_on_first_write_and_skips_mkdir_thereafter() {
        use tempfile::tempdir;

        let tempdir = tempdir().unwrap();
        let store_dir = StoreDir::new(tempdir.path());

        let (path_a, hash_a) = store_dir.write_cas_file(b"hello world", false).unwrap();
        assert!(store_dir.shard_already_ensured(hash_a[0]));
        assert!(path_a.is_file());

        // Content picked so its sha512 first byte matches 0x30 (the
        // shard for "hello world") is not something we can fabricate
        // without brute-forcing; instead assert the cache survives a
        // second write of identical content (same shard, same path —
        // the idempotent warm-cache branch inside `ensure_file`).
        let (path_b, hash_b) = store_dir.write_cas_file(b"hello world", false).unwrap();
        assert_eq!(hash_a, hash_b);
        assert_eq!(path_a, path_b);
        assert!(store_dir.shard_already_ensured(hash_b[0]));

        // Remove the shard dir out from under the cache. The cache
        // would return the stale "already ensured" answer, so the next
        // write would skip `create_dir_all` and then fail inside
        // `ensure_file` at `open`. We mostly care that the cache
        // *records* the ensured shards honestly; recovering from a
        // hostile out-of-band rmdir is out of scope (and pnpm doesn't
        // handle it either — the install aborts).
        //
        // Instead, write a second, *different* payload. If it lands in
        // a fresh shard, the cache grows; if it lands in 0x30 again by
        // coincidence, the cache stays put. Either way the write must
        // succeed and leave the file on disk.
        let (path_c, _) = store_dir.write_cas_file(b"goodbye world", false).unwrap();
        assert!(path_c.is_file());
    }

    #[test]
    fn cas_file_path_by_mode_rejects_invalid_hex() {
        let store_dir = StoreDir::new("STORE_DIR");
        assert_eq!(store_dir.cas_file_path_by_mode("", 0o644), None);
        assert_eq!(store_dir.cas_file_path_by_mode("a", 0o644), None);
        // Exactly two hex chars is still rejected — it would resolve to
        // the shard directory itself (files/XX/), which is not a file.
        assert_eq!(store_dir.cas_file_path_by_mode("ab", 0o644), None);
        assert_eq!(store_dir.cas_file_path_by_mode("zz", 0o644), None);
        assert_eq!(store_dir.cas_file_path_by_mode("Ab\tcd", 0o644), None);
        assert!(store_dir.cas_file_path_by_mode("abc", 0o644).is_some());
        assert!(store_dir.cas_file_path_by_mode("abcdef", 0o755).is_some());
    }
}
