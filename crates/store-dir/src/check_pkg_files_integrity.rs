//! Port of pnpm v11's
//! [`store/cafs/src/checkPkgFilesIntegrity.ts`](https://github.com/pnpm/pnpm/blob/main/store/cafs/src/checkPkgFilesIntegrity.ts).
//!
//! The store index's `package_index` row lists the CAFS paths a package
//! expanded into. Before reusing the row the caller checks those files
//! are still on disk and still match the recorded digests. This module
//! implements that check — with a fast path that skips filesystem work
//! entirely when the caller opted out of integrity verification.
//!
//! Mirrors the upstream structure function-for-function so a future
//! cross-reference (or a pnpm-side change we need to match) stays
//! cheap.

use crate::{CafsFileInfo, PackageFilesIndex, StoreDir};
use sha2::{Digest, Sha512};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// `in-tarball filename` → `CAFS path`. Return value of the two verify
/// entry points below.
pub type FilesMap = HashMap<String, PathBuf>;

/// Result of a `PackageFilesIndex`-row verification pass.
///
/// Mirrors pnpm's `VerifyResult`. `passed` is `false` if any referenced
/// CAFS file is missing, its size disagrees with the index, or its
/// content hash fails to match — the caller treats that as "this store
/// entry is stale, fall through to a fresh fetch". `files_map` is
/// populated either way so the caller can log or short-circuit without
/// re-walking the entry.
#[derive(Debug)]
pub struct VerifyResult {
    pub passed: bool,
    pub files_map: FilesMap,
}

/// Fast path used when `verify-store-integrity` is `false`.
///
/// Port of pnpm's
/// [`buildFileMapsFromIndex`](https://github.com/pnpm/pnpm/blob/main/store/cafs/src/checkPkgFilesIntegrity.ts).
/// No stat syscalls — the caller trusts the index, and any missing /
/// corrupt CAFS file surfaces lazily at import time (pnpm's `linkOrCopy`
/// equivalent).
pub fn build_file_maps_from_index(store_dir: &StoreDir, entry: &PackageFilesIndex) -> VerifyResult {
    let mut files_map = HashMap::with_capacity(entry.files.len());
    for (filename, info) in &entry.files {
        let Some(path) = store_dir.cas_file_path_by_mode(&info.digest, info.mode) else {
            // A malformed digest (non-hex / too short) skips this file.
            // pnpm has no equivalent — its `getFilePathByModeInCafs` doesn't
            // validate. For pacquet's SQLite rows we prefer a safe drop
            // over a `panic!`, since the row can be rebuilt from the
            // next install.
            continue;
        };
        files_map.insert(filename.clone(), path);
    }
    VerifyResult { passed: true, files_map }
}

/// Careful path used when `verify-store-integrity` is `true` (pnpm's
/// default).
///
/// Port of pnpm's `checkPkgFilesIntegrity`. Per file:
///
/// 1. `fs::metadata` the on-disk path to get its mtime + size.
/// 2. If `mtime - checked_at > 100 ms`, the file has been touched since
///    we last verified it. Compare sizes: mismatch → delete and fail;
///    match → re-hash the contents and compare against the stored
///    digest, deleting on mismatch.
/// 3. If the mtime is within 100 ms of the stored `checked_at`, trust
///    the digest and skip the hash — matches pnpm's own comment: "we
///    assume nobody will manually remove a file in the store and create
///    a new one".
///
/// Missing on disk (`ENOENT`) fails the whole entry so the caller
/// re-fetches. Unlike the prior pacquet implementation this does *not*
/// reject non-regular-file dirents preemptively — the integrity hash
/// catches real corruption, and pnpm doesn't guard against it in this
/// function either.
pub fn check_pkg_files_integrity(store_dir: &StoreDir, entry: &PackageFilesIndex) -> VerifyResult {
    let mut all_verified = true;
    let mut files_map = HashMap::with_capacity(entry.files.len());
    // pnpm's `verifiedFilesCache: Set<string>` dedups within a single
    // entry. Two different in-tarball filenames can point at the same
    // CAFS blob (hash-collision-less dedup), so caching by digest spares
    // the second stat.
    let mut verified: HashSet<&str> = HashSet::new();
    for (filename, info) in &entry.files {
        let Some(path) = store_dir.cas_file_path_by_mode(&info.digest, info.mode) else {
            all_verified = false;
            continue;
        };
        files_map.insert(filename.clone(), path.clone());
        if verified.contains(info.digest.as_str()) {
            continue;
        }
        if verify_file(&path, info, &entry.algo) {
            verified.insert(info.digest.as_str());
        } else {
            all_verified = false;
        }
    }
    VerifyResult { passed: all_verified, files_map }
}

/// Port of pnpm's `verifyFile`. `true` when the on-disk file is either
/// unmodified since the last verified check or modified but still
/// content-hashes to the stored digest.
fn verify_file(path: &Path, info: &CafsFileInfo, algo: &str) -> bool {
    let Some((is_modified, size)) = check_file(path, info.checked_at) else {
        return false;
    };
    if !is_modified {
        return true;
    }
    if size != info.size {
        // Wrong size → content definitely changed. Remove so the next
        // caller fetches a clean copy. Best-effort: removal failures
        // don't bubble up, they'll just hit the same mismatch next run.
        let _ = fs::remove_file(path);
        return false;
    }
    let passed = verify_file_integrity(path, &info.digest, algo);
    if !passed {
        let _ = fs::remove_file(path);
    }
    passed
}

/// Port of pnpm's `checkFile`. `(is_modified, size)` on a live file,
/// `None` on `ENOENT`.
///
/// 100 ms of slack on the mtime comparison matches pnpm's threshold —
/// accounts for coarse mtime resolution on some filesystems plus the
/// ≤1 ms drift between when we recorded `checked_at` and when the kernel
/// actually stamped the inode. A missing `checked_at` deserializes as
/// `Option<u64>::None` and is treated as `0`, which forces a re-hash the
/// first time an old-format row is read (same as pnpm's `?? 0`).
fn check_file(path: &Path, checked_at: Option<u64>) -> Option<(bool, u64)> {
    let meta = fs::metadata(path).ok()?;
    let mtime_ms =
        meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis().min(u64::MAX as u128)
            as u64;
    let baseline = checked_at.unwrap_or(0);
    let is_modified = mtime_ms.saturating_sub(baseline) > 100;
    Some((is_modified, meta.len()))
}

/// Port of pnpm's `verifyFileIntegrity`. Reads the whole file, hashes
/// with `algo`, compares against the stored hex `digest`.
///
/// Only `sha512` is supported — pacquet always writes that algo in
/// [`StoreDir::write_cas_file`]. Any other algo falls through to
/// `false` ("treat as verification failure"), matching pnpm's own
/// unknown-algo behaviour.
fn verify_file_integrity(path: &Path, digest: &str, algo: &str) -> bool {
    if algo != "sha512" {
        return false;
    }
    let Ok(data) = fs::read(path) else {
        return false;
    };
    let computed = Sha512::digest(&data);
    format!("{computed:x}") == digest
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::{fs, io::Write, time::SystemTime};
    use tempfile::tempdir;

    /// Write `content` to the correct CAFS path under `store_dir` for
    /// the given hex digest. Returns the path.
    fn plant_cafs_file(store_dir: &StoreDir, digest: &str, mode: u32, content: &[u8]) -> PathBuf {
        let path = store_dir.cas_file_path_by_mode(digest, mode).expect("valid digest");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        f.sync_all().ok();
        path
    }

    fn sha512_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha512::digest(bytes))
    }

    fn now_ms() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
    }

    fn index_with(algo: &str, info: Vec<(&str, CafsFileInfo)>) -> PackageFilesIndex {
        PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: algo.to_string(),
            files: info.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            side_effects: None,
        }
    }

    fn info(digest: &str, size: u64, mode: u32, checked_at: Option<u64>) -> CafsFileInfo {
        CafsFileInfo { checked_at, digest: digest.to_string(), mode, size }
    }

    /// `build_file_maps_from_index` never stats the files. Nothing on
    /// disk → still returns a populated `files_map` with `passed = true`.
    #[test]
    fn fast_path_skips_filesystem_checks() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let digest = sha512_hex(b"dummy");
        let entry = index_with("sha512", vec![("index.js", info(&digest, 5, 0o644, None))]);
        let result = build_file_maps_from_index(&store_dir, &entry);
        assert!(result.passed, "fast path always passes");
        let path = result.files_map.get("index.js").expect("path inserted");
        assert!(!path.exists(), "no file was planted — fast path didn't care");
    }

    /// On-disk file is live, `checked_at` is far in the future so the
    /// 100 ms slack keeps the mtime delta negative and we take the
    /// "unmodified, trust the digest" branch — without any `fs::read`.
    ///
    /// We can't easily set `mtime` from the standard library, but
    /// `checked_at` in the row is caller-controlled, so setting it
    /// above the real `mtime` is enough to exercise the trust path.
    #[test]
    fn careful_path_trusts_file_when_mtime_is_within_slack() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"hello, cafs";
        let digest = sha512_hex(content);
        let _path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let future = now_ms() + 3_600_000; // one hour from now
        let entry = index_with(
            "sha512",
            vec![("index.js", info(&digest, content.len() as u64, 0o644, Some(future)))],
        );
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(result.passed);
        assert_eq!(result.files_map.len(), 1);
    }

    /// Missing on disk → whole entry fails so the caller re-fetches.
    /// `files_map` is still populated for diagnostics.
    #[test]
    fn careful_path_fails_on_missing_cafs_file() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let digest = sha512_hex(b"nope");
        let entry = index_with("sha512", vec![("README", info(&digest, 4, 0o644, None))]);
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(!result.passed, "missing file → fail");
        assert_eq!(result.files_map.len(), 1);
    }

    /// File is on disk, the row claims the digest is for *different*
    /// bytes, size matches. `checked_at = None` ≡ 0, so the mtime-slack
    /// delta is "definitely > 100 ms", forcing re-hash → mismatch →
    /// `remove_file` + fail. Ports pnpm's `verifyFile` wrong-digest
    /// branch.
    #[test]
    fn careful_path_removes_file_whose_content_hash_mismatches() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let fake_digest = sha512_hex(b"claimed content");
        let actual = b"actual bytes!!!";
        let path = plant_cafs_file(&store_dir, &fake_digest, 0o644, actual);
        let entry = index_with(
            "sha512",
            vec![("whatever", info(&fake_digest, actual.len() as u64, 0o644, Some(0)))],
        );
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(!result.passed, "bad hash → fail");
        assert!(!path.exists(), "mismatched file is removed so the next call re-fetches");
    }

    /// Row claims size 999 but the file has 14 bytes. `checked_at = 0`
    /// puts us firmly in the "modified" branch (mtime now > 100 ms past
    /// 0). Size mismatch short-circuits before any re-hash. Ports
    /// pnpm's `currentFile.size !== fstat.size` branch.
    #[test]
    fn careful_path_removes_file_whose_size_mismatches_after_touch() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"actual content";
        let digest = sha512_hex(content);
        let path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let entry = index_with("sha512", vec![("mismatch", info(&digest, 999, 0o644, Some(0)))]);
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(!result.passed);
        assert!(!path.exists(), "size mismatch removes the file so a re-fetch starts clean");
    }

    /// Two filenames pointing at the same CAFS path verify once, not
    /// twice. Ports the `verifiedFilesCache` behaviour.
    #[test]
    fn careful_path_dedups_by_digest_within_a_single_entry() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"shared blob";
        let digest = sha512_hex(content);
        let _path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let future = now_ms() + 3_600_000;
        let info_shared = info(&digest, content.len() as u64, 0o644, Some(future));
        let entry = index_with(
            "sha512",
            vec![("a.txt", info_shared.clone_for_test()), ("b.txt", info_shared.clone_for_test())],
        );
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(result.passed);
        assert_eq!(result.files_map.len(), 2);
    }

    /// Unknown algorithm in the row → treat as verification failure,
    /// matching pnpm's "catch any crypto error, return false". The row
    /// is on disk, the mtime delta forces re-hash, and `verify_file_integrity`
    /// returns `false` because the algo isn't sha512.
    #[test]
    fn careful_path_fails_unknown_algo_as_verification_failure() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"bytes";
        let digest = sha512_hex(content);
        let path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let entry =
            index_with("sha256", vec![("x", info(&digest, content.len() as u64, 0o644, Some(0)))]);
        let result = check_pkg_files_integrity(&store_dir, &entry);
        assert!(!result.passed);
        assert!(!path.exists(), "unknown algo → treated as corrupt → removed");
    }

    // `CafsFileInfo` is `!Clone` in production (no need there). Give
    // the tests an explicit helper so each assertion builds its own
    // copy without implying a production `Clone` impl.
    impl CafsFileInfo {
        fn clone_for_test(&self) -> Self {
            Self {
                checked_at: self.checked_at,
                digest: self.digest.clone(),
                mode: self.mode,
                size: self.size,
            }
        }
    }
}
