//! Port of pnpm v11's
//! [`store/cafs/src/checkPkgFilesIntegrity.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/store/cafs/src/checkPkgFilesIntegrity.ts).
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
use dashmap::DashSet;
use sha2::{Digest, Sha512};
use std::{
    collections::HashMap,
    fs,
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

/// Set of CAFS paths whose on-disk integrity has already been verified
/// during the current install. Mirrors pnpm's
/// [`verifiedFilesCache: Set<string>`](https://github.com/pnpm/pnpm/blob/main/store/cafs/src/checkPkgFilesIntegrity.ts):
/// the caller threads one cache through every
/// [`check_pkg_files_integrity`] invocation so a CAFS blob that has
/// already been verified by package A doesn't get stat'd / re-hashed
/// again by package B.
///
/// Concurrent: the install fans `check_pkg_files_integrity` calls out
/// across tokio's blocking pool, so the cache must tolerate parallel
/// readers and writers. `DashSet` gives us that without any external
/// locking. Race-window duplicate verifies are benign (the `verify_file`
/// path is idempotent) and rare in practice.
pub type VerifiedFilesCache = DashSet<PathBuf>;

/// Shared handle to a [`VerifiedFilesCache`] — what every install-scope
/// caller passes around. `Arc` so the same cache survives across the
/// lockfile-driven and registry-driven install loops without
/// per-call clones, and so the value lives long enough to outlive the
/// individual `tokio::task::spawn_blocking` closures the verifier
/// dispatches into.
pub type SharedVerifiedFilesCache = Arc<VerifiedFilesCache>;

/// `in-tarball filename` → `CAFS path`. Return value of the two verify
/// entry points below.
pub type FilesMap = HashMap<String, PathBuf>;

/// Result of a `PackageFilesIndex`-row verification pass.
///
/// Mirrors pnpm's `VerifyResult`. `passed` is `false` if any referenced
/// CAFS file is missing, its size disagrees with the index, or its
/// content hash fails to match — the caller treats that as "this store
/// entry is stale, fall through to a fresh fetch". `files_map` is
/// returned either way as a best-effort `in-tarball filename` → `CAFS
/// path` map; it may be partial or empty when a digest in the index
/// row couldn't be reconstructed into a CAFS path, so callers should
/// gate reuse on `passed` rather than on the map's size.
#[derive(Debug)]
pub struct VerifyResult {
    pub passed: bool,
    pub files_map: FilesMap,
}

/// Fast path used when `verify-store-integrity` is `false`.
///
/// Port of pnpm's
/// [`buildFileMapsFromIndex`](https://github.com/pnpm/pnpm/blob/1819226b51/store/cafs/src/checkPkgFilesIntegrity.ts).
/// No stat syscalls — the caller trusts the index, and any missing /
/// corrupt CAFS file surfaces lazily at import time (pnpm's `linkOrCopy`
/// equivalent).
pub fn build_file_maps_from_index(store_dir: &StoreDir, entry: PackageFilesIndex) -> VerifyResult {
    let mut files_map = HashMap::with_capacity(entry.files.len());
    let mut passed = true;
    // Consume `entry.files` so the owned `String` filenames move into
    // `files_map` without a per-file clone. On a realistic install the
    // previous borrow-then-clone cost one allocation per file on every
    // warm cache hit.
    for (filename, info) in entry.files {
        let Some(path) = store_dir.cas_file_path_by_mode(&info.digest, info.mode) else {
            // A malformed digest (non-hex / too short) makes this entry
            // unreconstructable. pnpm's `getFilePathByModeInCafs` doesn't
            // validate and would crash at import time, so a `None` here
            // is pacquet-specific guardrail. We'd rather silently drop
            // the row than panic, but a partial `files_map` would leave
            // the caller with a cache hit missing package files — the
            // caller would proceed to link and end up with a broken
            // install. Flipping `passed` to `false` sends the whole
            // entry back through the re-fetch path so the install stays
            // consistent.
            tracing::debug!(
                target: "pacquet::store_index",
                ?filename,
                digest = %info.digest,
                "malformed CAFS digest in store-index row; re-fetching",
            );
            passed = false;
            continue;
        };
        files_map.insert(filename, path);
    }
    VerifyResult { passed, files_map }
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
pub fn check_pkg_files_integrity(
    store_dir: &StoreDir,
    entry: PackageFilesIndex,
    verified_files_cache: &VerifiedFilesCache,
) -> VerifyResult {
    // Destructure so the owned `files` HashMap and `algo` String can be
    // consumed below; moving beats the extra per-file `filename.clone()`
    // the old borrow-based signature forced on the hot path.
    let PackageFilesIndex { files, algo, .. } = entry;
    let mut all_verified = true;
    let mut files_map = HashMap::with_capacity(files.len());
    // `verified_files_cache` is the install-scoped
    // [`VerifiedFilesCache`] — pnpm's `verifiedFilesCache: Set<string>`.
    // Threading it through every call dedups across packages, not just
    // within one entry: a CAFS blob seen by package A's verify pass
    // skips the stat / re-hash when package B references it later.
    //
    // Key the set by the resolved CAFS path, not by `info.digest`. The
    // path factors in `info.mode` (via `-exec` suffix for executables
    // in `cas_file_path_by_mode`), so the same content digest can
    // legitimately appear under two distinct on-disk paths when the
    // tarball ships it with different executable bits. Digest-only
    // dedup would skip verifying the second path and happily return
    // `passed: true` with a stale / missing blob still on disk.
    for (filename, info) in files {
        let Some(path) = store_dir.cas_file_path_by_mode(&info.digest, info.mode) else {
            tracing::debug!(
                target: "pacquet::store_index",
                ?filename,
                digest = %info.digest,
                "malformed CAFS digest in store-index row; re-fetching",
            );
            all_verified = false;
            continue;
        };
        if !verified_files_cache.contains(&path) {
            if verify_file(&path, &filename, &info, &algo) {
                // One `PathBuf` clone per unique CAFS path we actually
                // verified; zero for dedup hits. Strictly better than
                // the per-filename clone the borrow-based version had.
                //
                // Concurrency note: another thread may verify the same
                // path between the `contains` check and our `insert`,
                // doing the stat twice. That's benign — `verify_file`
                // is idempotent and the cache converges to the same
                // state either way. Pnpm's worker_threads cache has
                // the same race-window for the same reason.
                verified_files_cache.insert(path.clone());
            } else {
                all_verified = false;
            }
        }
        files_map.insert(filename, path);
    }
    VerifyResult { passed: all_verified, files_map }
}

/// Port of pnpm's `verifyFile`. `true` when the on-disk file is either
/// unmodified since the last verified check or modified but still
/// content-hashes to the stored digest.
///
/// `filename` is the in-tarball path the caller is trying to reuse; it
/// doesn't affect behaviour, only the `debug!` log when verification
/// fails, so operators can see *which* package file invalidated the
/// store-index row in the log.
fn verify_file(path: &Path, filename: &str, info: &CafsFileInfo, algo: &str) -> bool {
    let Some((is_modified, size)) = check_file(path, info.checked_at) else {
        tracing::debug!(
            target: "pacquet::store_index",
            ?filename,
            ?path,
            "CAFS file missing or unreadable; re-fetching",
        );
        return false;
    };
    if !is_modified {
        return true;
    }
    if size != info.size {
        // Wrong size → content definitely changed. Remove so the next
        // caller fetches a clean copy. See `remove_stale_cafs_entry`
        // for why this has to cover dirs too.
        tracing::debug!(
            target: "pacquet::store_index",
            ?filename,
            ?path,
            expected_size = info.size,
            actual_size = size,
            "CAFS file size mismatch; scrubbing and re-fetching",
        );
        remove_stale_cafs_entry(path);
        return false;
    }
    let passed = verify_file_integrity(path, &info.digest, algo);
    if !passed {
        tracing::debug!(
            target: "pacquet::store_index",
            ?filename,
            ?path,
            "CAFS file digest mismatch or unknown algo; scrubbing and re-fetching",
        );
        remove_stale_cafs_entry(path);
    }
    passed
}

/// Remove a CAFS dirent that failed verification, matching pnpm's
/// `rimrafSync` semantics.
///
/// `fs::remove_file` on a directory returns `EISDIR` / `EPERM`, and a
/// corrupted store that has a directory sitting where a CAFS blob
/// belongs (stray `mkdir -p`, interrupted write, filesystem hiccup)
/// would stay there forever if we only tried `remove_file`. Next
/// install's verification would fail again and again — the store
/// wouldn't self-heal.
///
/// Best-effort for both: try `remove_file`, fall back to
/// `remove_dir_all` if the dirent is a directory. Errors are logged at
/// `debug` and dropped — worst case the next install notices the same
/// stale dirent and retries. We use `symlink_metadata` so we identify
/// the dirent type without following a symlink.
fn remove_stale_cafs_entry(path: &Path) {
    let is_dir = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir());
    let result = if is_dir { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    if let Err(error) = result {
        tracing::debug!(
            target: "pacquet::store_index",
            ?path,
            ?error,
            "failed to scrub stale CAFS entry; next install will retry",
        );
    }
}

/// Port of pnpm's `checkFile`. `Some((is_modified, size))` for a file
/// we can read metadata for; `None` otherwise.
///
/// Pnpm rethrows non-`ENOENT` errors and only returns `null` for
/// `ENOENT`. This port collapses every metadata error (permission
/// denied, EIO, platform mtime representation failures) to `None`
/// instead, which the caller then treats as "verification failed →
/// re-fetch". That's a safer default for a cache-hint path — we don't
/// want a transient `EACCES` on a CAS blob to panic the install — and
/// the content-hash check in `verify_file_integrity` still catches
/// actual corruption. If we ever want pnpm-strict error propagation,
/// changing the return type to `Result<Option<…>>` is the right shape.
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

/// Port of pnpm's `verifyFileIntegrity`. Streams the file through the
/// hasher in 64 KiB chunks and compares the digest against the stored
/// hex `digest`.
///
/// pnpm itself calls `readFileSync` + `crypto.hash`, which loads the
/// whole blob into a `Buffer` first. On Node that's capped implicitly
/// by `Buffer.kMaxLength`; in Rust we'd allocate the full file up
/// front, spiking RSS for multi-MB CAS blobs when an install is
/// verifying many entries in parallel. A `BufReader` + incremental
/// `Digest::update` is equivalent on the wire and keeps peak memory
/// bounded per thread.
///
/// Only `sha512` is supported — pacquet always writes that algo in
/// [`StoreDir::write_cas_file`]. Any other algo falls through to
/// `false` ("treat as verification failure"), matching pnpm's own
/// unknown-algo behaviour. An I/O error mid-read also falls through to
/// `false` so the caller re-fetches rather than deciding on a partial
/// hash.
fn verify_file_integrity(path: &Path, digest: &str, algo: &str) -> bool {
    if algo != "sha512" {
        return false;
    }
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha512::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            // `Interrupted` is the one error we retry — it's a signal,
            // not a real IO failure. Everything else (NotFound, EIO,
            // PermissionDenied, …) short-circuits to `false` so the
            // caller re-fetches.
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    format!("{:x}", hasher.finalize()) == digest
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

    /// `build_file_maps_from_index` never stats the files. With a
    /// valid digest, it returns a populated `files_map` with
    /// `passed = true` regardless of whether anything is on disk —
    /// the sibling `fast_path_fails_when_digest_is_malformed` covers
    /// the "digest was not resolvable" failure case.
    #[test]
    fn fast_path_skips_filesystem_checks() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let digest = sha512_hex(b"dummy");
        let entry = index_with("sha512", vec![("index.js", info(&digest, 5, 0o644, None))]);
        let result = build_file_maps_from_index(&store_dir, entry);
        assert!(result.passed, "fast path passes for a valid digest without touching the disk");
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
        assert!(result.passed);
        assert_eq!(result.files_map.len(), 2);
    }

    /// A CAFS path verified during one `check_pkg_files_integrity` call
    /// must not be re-verified by the next call when both share the
    /// same `VerifiedFilesCache`. Ports pnpm's install-scoped
    /// `verifiedFilesCache: Set<string>` semantics.
    ///
    /// The proof: plant the file, run a successful first verify against
    /// it (populates the cache), then *delete* the file and run a
    /// second verify. If the cache short-circuits the second call, it
    /// returns `passed: true` despite the missing file — that's the
    /// observable signal that the stat was skipped. Real installs
    /// don't delete files mid-install, so this artificial setup is
    /// purely a test handle for the dedup behaviour.
    #[test]
    fn careful_path_dedups_across_calls_via_shared_cache() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"shared-across-packages";
        let digest = sha512_hex(content);
        let path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let future = now_ms() + 3_600_000;
        let info_shared = info(&digest, content.len() as u64, 0o644, Some(future));

        let cache = VerifiedFilesCache::new();

        let entry_a = index_with("sha512", vec![("a-pkg/index.js", info_shared.clone_for_test())]);
        let result_a = check_pkg_files_integrity(&store_dir, entry_a, &cache);
        assert!(result_a.passed, "first call verifies the live file");
        assert!(cache.contains(&path), "successful verify populates the shared cache");

        // Pull the rug out from under the second call. Without the
        // shared cache we'd stat-and-fail; with it, the path is
        // already in `cache` so the inner `verify_file` is skipped.
        std::fs::remove_file(&path).unwrap();
        let entry_b = index_with("sha512", vec![("b-pkg/index.js", info_shared.clone_for_test())]);
        let result_b = check_pkg_files_integrity(&store_dir, entry_b, &cache);
        assert!(
            result_b.passed,
            "second call should short-circuit via the shared cache and skip the now-missing file",
        );
    }

    /// Same digest with different `mode` resolves to two distinct CAFS
    /// paths (`<hex>` vs `<hex>-exec`). Keying dedup by digest alone
    /// would skip verifying the second path — this test plants only
    /// the non-exec half and asserts the install still fails
    /// verification, forcing a re-fetch, instead of returning
    /// `passed: true` with a missing exec blob.
    #[test]
    fn careful_path_dedups_per_resolved_path_not_per_digest() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let content = b"polymode";
        let digest = sha512_hex(content);
        // Plant the non-exec variant only; leave the exec path missing.
        let non_exec_path = plant_cafs_file(&store_dir, &digest, 0o644, content);
        let exec_path = store_dir.cas_file_path_by_mode(&digest, 0o755).unwrap();
        assert!(!exec_path.exists());
        assert_ne!(non_exec_path, exec_path);

        let future = now_ms() + 3_600_000;
        let entry = index_with(
            "sha512",
            vec![
                ("lib.js", info(&digest, content.len() as u64, 0o644, Some(future))),
                ("bin/app", info(&digest, content.len() as u64, 0o755, Some(future))),
            ],
        );
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
        assert!(
            !result.passed,
            "same digest + different mode = different CAFS path; missing exec blob must fail",
        );
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
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
        assert!(!result.passed);
        assert!(!path.exists(), "unknown algo → treated as corrupt → removed");
    }

    /// A CAFS dirent that's a directory (store corruption — stray
    /// `mkdir -p` or interrupted write) must not survive verification:
    /// pacquet used to reject with `remove_file(dir)` → `EISDIR`, which
    /// silently failed and left the directory in place forever. The new
    /// `remove_stale_cafs_entry` falls back to `remove_dir_all` so the
    /// store actually self-heals on the next install.
    #[test]
    fn careful_path_removes_directory_at_cafs_path() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        // Plant a directory where a CAFS file belongs.
        let digest = "c".repeat(128);
        let cafs_path = store_dir.cas_file_path_by_mode(&digest, 0o644).unwrap();
        fs::create_dir_all(&cafs_path).unwrap();
        // Row claims non-zero size; `check_file` stats the dir, size
        // mismatches the row, we hit the `remove_stale_cafs_entry` path.
        let entry =
            index_with("sha512", vec![("impostor", info(&digest, 1_000_000, 0o644, Some(0)))]);
        let result = check_pkg_files_integrity(&store_dir, entry, &VerifiedFilesCache::new());
        assert!(!result.passed);
        assert!(
            !cafs_path.exists(),
            "a directory at the CAFS path must be scrubbed like a file so the next install re-fetches",
        );
    }

    /// `build_file_maps_from_index` shouldn't silently drop unresolvable
    /// entries — that would give the caller a partial `files_map` and a
    /// cache hit with missing files. Flip `passed` to `false` when any
    /// digest can't be turned into a CAFS path so the caller re-fetches.
    #[test]
    fn fast_path_fails_when_digest_is_malformed() {
        let tmp = tempdir().unwrap();
        let store_dir = StoreDir::new(tmp.path());
        let entry = index_with("sha512", vec![("bad-digest", info("not-hex", 10, 0o644, None))]);
        let result = build_file_maps_from_index(&store_dir, entry);
        assert!(!result.passed, "malformed digest → whole entry fails so caller re-fetches");
        assert_eq!(result.files_map.len(), 0);
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
