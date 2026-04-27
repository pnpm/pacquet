use pacquet_store_dir::StoreIndex;
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use tempfile::{TempDir, tempdir};

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

/// Pin `walk_reqwest_chain`'s contract: a `NetworkError` formed
/// from a real reqwest connect failure must surface the leaf
/// reason (e.g. `Connection refused`) appended to the wrapper
/// message, not stop at reqwest's `error sending request for url
/// (URL)`. Without the helper, the user sees only the wrapper —
/// which is what triggered the original "what's actually failing?"
/// debugging round on this branch.
///
/// Uses `127.0.0.1:1` (port 1 is reserved; connect always fails
/// with a deterministic ECONNREFUSED on every host I've tried)
/// and `fast_fail_client`'s 1 s bounds, so the test stays
/// hermetic and quick.
#[tokio::test]
async fn network_error_display_includes_reqwest_inner_chain() {
    let url = "http://127.0.0.1:1/whatever";
    let client = fast_fail_client();
    let err =
        client.acquire().await.get(url).send().await.expect_err("connecting to port 1 must fail");
    let net_err = NetworkError { url: url.to_string(), error: err };

    let rendered = net_err.to_string();
    assert!(
        rendered.starts_with("Failed to fetch http://127.0.0.1:1/"),
        "wrapper prefix missing, got: {rendered:?}",
    );

    // Reqwest's wrapper already includes the URL in `(...)`; the
    // leaf reason appears after the wrapper, separated by `: `.
    // Assert there *is* a non-empty frame after that — without
    // `walk_reqwest_chain`, this is exactly what got dropped.
    let leaf_section = rendered
        .split_once("error sending request for url (")
        .and_then(|(_, rest)| rest.split_once(")"))
        .map(|(_, after_paren)| after_paren)
        .expect("rendered output should include reqwest's wrapper");
    assert!(
        !leaf_section.trim().is_empty(),
        "expected leaf cause appended after reqwest wrapper, got: {rendered:?}",
    );
    assert!(
        leaf_section.starts_with(": "),
        "leaf should be joined with `: ` per walk_reqwest_chain, got: {rendered:?}",
    );

    // Structural form for completeness — `#[error(source)]` should
    // expose the reqwest::Error so miette / `Error::source` can
    // walk into it independently of our flattened Display.
    assert!(
        std::error::Error::source(&net_err).is_some(),
        "NetworkError should expose its reqwest::Error as source",
    );
}

/// Default `RetryOpts` for unit tests. We don't want the suite to
/// sit through pnpm's 10 s + 60 s production backoff just to assert
/// that an unreachable URL eventually fails — every test that
/// exercises a network call here either short-circuits to a cache
/// hit or expects the failure path. `retries: 0` keeps the failure
/// path deterministic and bounded by `fast_fail_client`'s 1 s
/// timeouts; tests that specifically want to *prove* the retry
/// loop runs should construct their own [`RetryOpts`].
fn test_retry_opts() -> RetryOpts {
    RetryOpts { retries: 0, ..RetryOpts::default() }
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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
        CafsFileInfo { digest: format!("{bin_hash:x}"), mode: 0o755, size: 39, checked_at: None },
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
    }
    .run_without_mem_cache()
    .await
    .expect("cache hit should succeed without network");

    assert_eq!(cas_paths.len(), 2);
    assert_eq!(cas_paths.get("package.json"), Some(&pkg_json_path));
    assert_eq!(cas_paths.get("bin/cli.js"), Some(&bin_path));

    drop(store_dir);
}

/// When `prefetched_cas_paths` already covers the requested
/// `(integrity, pkg_id)`, `run_without_mem_cache` must short-circuit
/// to the prefetched map and never touch the SQLite index or the
/// network. `store_index: None` proves it doesn't fall through to
/// the per-snapshot SQLite lookup, and the unreachable
/// `package_url` proves the network path is also bypassed.
#[tokio::test]
async fn reuses_prefetched_cas_paths_when_provided() {
    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
    let pkg_id = "fake@1.0.0";
    let cache_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

    // Synthetic cas-path map — its values just need to be returned
    // verbatim by the prefetched short-circuit. They don't need to
    // resolve to anything on disk because no integrity check runs
    // on this path.
    let mut files: HashMap<String, PathBuf> = HashMap::new();
    files.insert("package.json".to_string(), PathBuf::from("/synthetic/package.json"));
    files.insert("bin/cli.js".to_string(), PathBuf::from("/synthetic/bin/cli.js"));
    let mut prefetched: PrefetchedCasPaths = HashMap::new();
    prefetched.insert(cache_key, Arc::new(files.clone()));

    // Use a leaked tempdir for `store_dir` so the helper has
    // somewhere to point even though we never read it.
    let (_keep, store_path) = tempdir_with_leaked_path();

    let cas_paths = DownloadTarballToStore {
        http_client: &fast_fail_client(),
        store_dir: store_path,
        // No SQLite handle: any fall-through to the per-snapshot
        // SQLite lookup would just miss, so a network attempt
        // would follow — and that would fail against the
        // unreachable URL below, failing the test.
        store_index: None,
        store_index_writer: None,
        verify_store_integrity: true,
        package_integrity: &pkg_integrity,
        package_unpacked_size: None,
        package_url: "http://127.0.0.1:1/unreachable.tgz",
        package_id: pkg_id,
        prefetched_cas_paths: Some(&prefetched),
        retry_opts: test_retry_opts(),
    }
    .run_without_mem_cache()
    .await
    .expect("prefetched short-circuit should succeed without network");

    assert_eq!(cas_paths.len(), 2);
    assert_eq!(cas_paths.get("package.json"), files.get("package.json"));
    assert_eq!(cas_paths.get("bin/cli.js"), files.get("bin/cli.js"));
}

/// `prefetch_cas_paths` against an index row whose CAFS blobs
/// exist on disk and verify cleanly must return a hit for the
/// requested key. Mirrors the warm-cache install shape: we
/// pre-write a row, then ask the prefetch to look it up.
#[tokio::test]
async fn prefetch_cas_paths_returns_hits_for_live_index_rows() {
    let (store_dir, store_path) = tempdir_with_leaked_path();

    let (pkg_json_path, pkg_json_hash) =
        store_path.write_cas_file(b"{\"name\":\"fake\"}", false).unwrap();

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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

    let prefetched = prefetch_cas_paths(
        StoreIndex::shared_readonly_in(store_path),
        store_path,
        vec![index_key.clone()],
        true,
    )
    .await;

    let map = prefetched.get(&index_key).expect("hit");
    assert_eq!(map.get("package.json"), Some(&pkg_json_path));
    drop(store_dir);
}

/// `prefetch_cas_paths` must omit entries whose integrity check
/// fails — same policy as the per-snapshot `load_cached_cas_paths`
/// path. We seed an index row that points at a digest no file on
/// disk matches; the prefetch should drop the row from its result
/// rather than return a half-populated map (which would mislead
/// the warm-batch path into thinking the package was ready).
#[tokio::test]
async fn prefetch_cas_paths_omits_failed_integrity_entries() {
    let (store_dir, store_path) = tempdir_with_leaked_path();

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
    let pkg_id = "fake@1.0.0";
    let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

    let mut files = HashMap::new();
    files.insert(
        "package.json".to_string(),
        CafsFileInfo {
            // Digest of a file that was never written to disk.
            digest: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string(),
            mode: 0o644,
            size: 15,
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

    let prefetched = prefetch_cas_paths(
        StoreIndex::shared_readonly_in(store_path),
        store_path,
        vec![index_key.clone()],
        // Verification on: the missing CAFS blob trips
        // `check_pkg_files_integrity`'s "scrub & re-fetch" path,
        // which turns the row into a miss.
        true,
    )
    .await;

    assert!(
        !prefetched.contains_key(&index_key),
        "row that fails integrity must not appear in prefetch result",
    );
    drop(store_dir);
}

/// With `verify_store_integrity = false`, `prefetch_cas_paths`
/// goes through `build_file_maps_from_index` instead of
/// `check_pkg_files_integrity` — the index row is trusted and
/// no `fs::metadata` syscalls run per file. The result must
/// still surface an entry for the requested key, even when no
/// CAFS blob exists on disk; correctness is left to the caller's
/// downstream import step (matches pnpm's behaviour with
/// `verify-store-integrity: false`).
#[tokio::test]
async fn prefetch_cas_paths_skips_filesystem_checks_when_verify_disabled() {
    let (store_dir, store_path) = tempdir_with_leaked_path();

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
    let pkg_id = "fake@1.0.0";
    let index_key = store_index_key(&pkg_integrity.to_string(), pkg_id);

    let mut files = HashMap::new();
    files.insert(
        "package.json".to_string(),
        CafsFileInfo {
            // Digest matches no on-disk file, but with
            // `verify_store_integrity = false` we never check.
            digest: "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".to_string(),
            mode: 0o644,
            size: 15,
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

    let prefetched = prefetch_cas_paths(
        StoreIndex::shared_readonly_in(store_path),
        store_path,
        vec![index_key.clone()],
        false,
    )
    .await;

    let map = prefetched.get(&index_key).expect(
        "verify=false should trust the index row and surface the entry without checking disk",
    );
    assert!(map.contains_key("package.json"));
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

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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

    let pkg_integrity = integrity(
        "sha512-q/IXcMGuF8v7ZLf/JeYfE/pB4Wg1yxT6jXJz8JxRK7a4mJSXV1QKMXDPfZkvMHTZpYxWBDoJiXtptDWFnoCA2w==",
    );
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
        prefetched_cas_paths: None,
        retry_opts: test_retry_opts(),
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

/// `RetryOpts::default()` reproduces pnpm's
/// `network/fetch/src/fetch.ts` defaults: 2 retries, factor 10,
/// minTimeout 10 s, maxTimeout 60 s. The first post-failure delay
/// is `minTimeout`; subsequent delays multiply by `factor` until
/// they hit `maxTimeout`.
#[test]
fn retry_opts_delay_matches_pnpm_formula() {
    let opts = RetryOpts::default();
    assert_eq!(opts.delay_for(0), Duration::from_millis(10_000));
    // 10s * 10 = 100s, capped at 60s
    assert_eq!(opts.delay_for(1), Duration::from_millis(60_000));
    assert_eq!(opts.delay_for(5), Duration::from_millis(60_000));
}

/// Pathological `attempt` values must not panic / overflow. The
/// retry loop uses `attempt: u32`, so the worst case in production
/// is bounded by `retries`, but we want the math to stay sound
/// regardless.
#[test]
fn retry_opts_delay_does_not_overflow() {
    let opts = RetryOpts::default();
    assert_eq!(opts.delay_for(u32::MAX), Duration::from_millis(60_000));
}

/// pnpm's
/// [`remoteTarballFetcher.ts`](https://github.com/pnpm/pnpm/blob/1819226b51/fetching/tarball-fetcher/src/remoteTarballFetcher.ts#L76-L84)
/// rejects only HTTP 401, 403, 404 (and the git-prepare error code,
/// which doesn't apply to registry tarballs). Every other failure
/// — arbitrary 4xx, 5xx, network reset, integrity mismatch, gzip
/// or tar parse error — falls through to `op.retry(error)` and is
/// retried. Diverging here was the original bug behind #259.
#[test]
fn retry_classification_matches_pnpm_policy() {
    let url = "https://example.test/pkg.tgz".to_string();
    let mk_http =
        |status: u16| TarballError::HttpStatus(HttpStatusError { url: url.clone(), status });

    // Fail-fast set — exactly the three codes pnpm short-circuits on.
    for code in [401u16, 403, 404] {
        assert!(!is_transient_error(&mk_http(code)), "HTTP {code} should fail fast");
    }
    // Everything else, including arbitrary 4xx that pnpm does not
    // single out, must retry.
    for code in [400u16, 408, 409, 410, 418, 420, 422, 429, 500, 502, 503, 504] {
        assert!(is_transient_error(&mk_http(code)), "HTTP {code} should retry");
    }

    // Non-HTTP failures: pnpm wraps body fetch + addFilesFromTarball
    // (integrity + extraction) in one retried closure, so anything
    // raised inside that closure retries. Cover a representative
    // sample.
    let bad_integrity: Integrity =
        "sha512-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa==".parse().unwrap();
    let ssri_err = bad_integrity.check(b"unrelated body").unwrap_err();
    let checksum =
        TarballError::Checksum(VerifyChecksumError { url: url.clone(), error: ssri_err });
    assert!(is_transient_error(&checksum), "integrity mismatch should retry");

    let too_large = TarballError::TarballTooLarge { url: url.clone(), advertised_size: u64::MAX };
    assert!(is_transient_error(&too_large), "TarballTooLarge should retry");
}

/// Real pnpm-published tarball (`@fastify/error@3.3.0`, 4.4 KiB).
/// Embedded so the retry-success test below has a body that
/// integrity-checks and extracts successfully on the retry attempt
/// — which is the only way to exercise the post-network steps of
/// the retry loop without going to the live registry.
const FASTIFY_ERROR_TARBALL: &[u8] =
    include_bytes!("../../../tasks/micro-benchmark/fixtures/@fastify+error-3.3.0.tgz");
const FASTIFY_ERROR_INTEGRITY: &str = "sha512-dj7vjIn1Ar8sVXj2yAXiMNCJDmS9MQ9XMlIecX2dIzzhjSHCyKo4DdXjXMs7wKW2kj6yvVRSpuQjOZ3YLrh56w==";

/// `RetryOpts` for the mockito tests below: keep the 2-retry budget
/// so we exercise the full attempt count, but collapse the backoff
/// to milliseconds so the test suite isn't sitting through pnpm's
/// production 10 s + 60 s waits.
fn fast_retry_opts() -> RetryOpts {
    RetryOpts {
        retries: 2,
        factor: 1,
        min_timeout: Duration::from_millis(1),
        max_timeout: Duration::from_millis(1),
    }
}

/// First request returns 503 (transient per pnpm's policy), the
/// retry returns 200 with the real fastify-error tarball. The
/// retry loop must drive the full pipeline — network → integrity
/// → extract — to completion on the second attempt, which is the
/// core fix for #259.
#[tokio::test]
async fn retries_then_succeeds_on_transient_5xx() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    let fail = server.mock("GET", "/pkg.tgz").with_status(503).expect(1).create_async().await;
    let ok = server
        .mock("GET", "/pkg.tgz")
        .with_status(200)
        .with_body(FASTIFY_ERROR_TARBALL)
        .expect(1)
        .create_async()
        .await;

    let url = format!("{}/pkg.tgz", server.url());
    let client = ThrottledClient::default();
    let pkg_integrity = integrity(FASTIFY_ERROR_INTEGRITY);

    let (cas_paths, _idx) = fetch_and_extract_with_retry(
        &client,
        &url,
        &pkg_integrity,
        None,
        store_path,
        fast_retry_opts(),
    )
    .await
    .expect("transient 503 should be followed by a successful retry");

    // Sanity-check: extraction actually populated the cas-paths map.
    assert!(cas_paths.contains_key("package.json"));
    fail.assert_async().await;
    ok.assert_async().await;
    drop(store_dir_keep);
}

/// pnpm's tarball fetcher retries integrity mismatches by re-running
/// the full `addFilesFromTarball` closure on the next attempt. With
/// a body that never matches the integrity hash, the loop must
/// retry until the budget is exhausted and then surface a
/// `Checksum` error — not fail fast on the first mismatch.
#[tokio::test]
async fn retries_integrity_mismatch_until_exhausted() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    // 2 retries + 1 initial = 3 attempts; every one returns the same
    // body, which the wrong integrity hash will reject.
    let mock = server
        .mock("GET", "/pkg.tgz")
        .with_status(200)
        .with_body(b"definitely not a tarball matching the digest below")
        .expect(3)
        .create_async()
        .await;

    let url = format!("{}/pkg.tgz", server.url());
    let client = ThrottledClient::default();
    // Real-format integrity, deliberately not matching the body above.
    let pkg_integrity = integrity(
        "sha512-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa==",
    );

    let err = fetch_and_extract_with_retry(
        &client,
        &url,
        &pkg_integrity,
        None,
        store_path,
        fast_retry_opts(),
    )
    .await
    .expect_err("integrity mismatch should exhaust the retry budget");
    assert!(matches!(err, TarballError::Checksum(_)), "expected Checksum error, got {err:?}",);
    mock.assert_async().await;
    drop(store_dir_keep);
}

/// 404 is in pnpm's no-retry set. `expect(1)` makes the test fail if
/// the retry loop fires a second request — that would mean we're
/// spinning on a permanently-missing tarball.
#[tokio::test]
async fn fails_fast_on_404() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    let mock = server.mock("GET", "/missing.tgz").with_status(404).expect(1).create_async().await;

    let url = format!("{}/missing.tgz", server.url());
    let client = ThrottledClient::default();
    let pkg_integrity = integrity(FASTIFY_ERROR_INTEGRITY);

    let err = fetch_and_extract_with_retry(
        &client,
        &url,
        &pkg_integrity,
        None,
        store_path,
        fast_retry_opts(),
    )
    .await
    .expect_err("404 must fail-fast without retry");
    match err {
        TarballError::HttpStatus(http) => assert_eq!(http.status, 404),
        other => panic!("expected HttpStatus(404), got: {other:?}"),
    }
    mock.assert_async().await;
    drop(store_dir_keep);
}

/// pnpm retries arbitrary 4xx codes that aren't 401/403/404 (any
/// FetchError throws to the outer catch, which only short-circuits
/// on the explicit no-retry set). 410 Gone is the canonical example
/// — semantically permanent but pnpm still hits it `retries+1` times.
#[tokio::test]
async fn retries_other_4xx_codes() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/pkg.tgz")
        .with_status(410)
        .expect(3) // retries: 2 + initial attempt = 3 total
        .create_async()
        .await;

    let url = format!("{}/pkg.tgz", server.url());
    let client = ThrottledClient::default();
    let pkg_integrity = integrity(FASTIFY_ERROR_INTEGRITY);

    let err = fetch_and_extract_with_retry(
        &client,
        &url,
        &pkg_integrity,
        None,
        store_path,
        fast_retry_opts(),
    )
    .await
    .expect_err("non-401/403/404 4xx should exhaust the retry budget");
    match err {
        TarballError::HttpStatus(http) => assert_eq!(http.status, 410),
        other => panic!("expected HttpStatus(410), got: {other:?}"),
    }
    mock.assert_async().await;
    drop(store_dir_keep);
}

/// Persistent 5xx must stop after `retries + 1` total tries. Pairs
/// with `retries_then_succeeds_on_transient_5xx` to bracket both
/// success and exhaustion paths.
#[tokio::test]
async fn retry_exhaustion_returns_last_error() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    let mock = server.mock("GET", "/pkg.tgz").with_status(500).expect(3).create_async().await;

    let url = format!("{}/pkg.tgz", server.url());
    let client = ThrottledClient::default();
    let pkg_integrity = integrity(FASTIFY_ERROR_INTEGRITY);

    let err = fetch_and_extract_with_retry(
        &client,
        &url,
        &pkg_integrity,
        None,
        store_path,
        fast_retry_opts(),
    )
    .await
    .expect_err("permanent 500s should exhaust the retry budget");
    match err {
        TarballError::HttpStatus(http) => assert_eq!(http.status, 500),
        other => panic!("expected HttpStatus(500), got: {other:?}"),
    }
    mock.assert_async().await;
    drop(store_dir_keep);
}

/// Regression test for the `run_with_mem_cache` deadlock that hung
/// `pacquet install` on real-network workloads at high concurrency.
/// The if-let branch used to hold a `DashMap::Ref` (a synchronous
/// shard read guard) across two `.await` points; under enough
/// concurrency another task on the same worker would call
/// `mem_cache.insert` for a key hashing to the same shard, block
/// on the parking_lot write, and starve every worker.
///
/// To reproduce end-to-end:
/// * Mockito serves the real fastify-error tarball with a
///   per-request sleep so the InProgress window is wide enough to
///   schedule the contending task.
/// * Two concurrent calls for the same URL: one wins the else
///   branch, the other parks in the if-let branch.
/// * A third call for a different URL whose key hashes to the same
///   DashMap shard. Its else branch calls `mem_cache.insert`, which
///   needs a write guard on the same shard.
/// * Single-worker tokio runtime: with the bug, the only worker
///   blocks on parking_lot's exclusive wait and nothing else can be
///   polled. The runtime is parked in a side OS thread so the test
///   asserts the deadlock as a wall-clock timeout instead of
///   hanging the test process forever.
#[test]
fn run_with_mem_cache_does_not_deadlock_on_dashmap_shard_contention() {
    use std::sync::mpsc;
    use std::thread;

    const RESPONSE_LATENCY: Duration = Duration::from_millis(300);
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("tarball-deadlock-regression".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("build single-worker runtime");

            rt.block_on(async {
                let mut server = mockito::Server::new_async().await;
                let url1 = format!("{}/pkg.tgz", server.url());

                // `DashMap::default()` uses `RandomState`, whose seed is
                // per-instance — so we MUST probe the very cache the
                // runtime tasks will use. A separate "probe" map would
                // hash to different shards and silently defeat the
                // collision setup, hiding the regression.
                let mem_cache: &'static MemCache = Box::leak(Box::new(MemCache::default()));
                let target_shard = mem_cache.determine_map(&url1);
                let url2 = (0u32..10_000)
                    .map(|i| format!("{}/pkg-{i}.tgz", server.url()))
                    .find(|u| u != &url1 && mem_cache.determine_map(u) == target_shard)
                    .expect("no colliding URL within 10000 candidates");

                let path1 = url1.trim_start_matches(server.url().as_str()).to_string();
                let path2 = url2.trim_start_matches(server.url().as_str()).to_string();
                // Both endpoints are expected to be hit exactly once: A
                // for url1, C for url2. B uses the in-memory cache and
                // never reaches the network. Asserting hit counts guards
                // against a future short-circuit (e.g. a store-index
                // cache hit) that would let `run_with_mem_cache` return
                // before the contention window we want to exercise.
                let slow1 = server
                    .mock("GET", path1.as_str())
                    .with_status(200)
                    .expect(1)
                    .with_chunked_body(|w| {
                        std::thread::sleep(RESPONSE_LATENCY);
                        w.write_all(FASTIFY_ERROR_TARBALL)
                    })
                    .create_async()
                    .await;
                let slow2 = server
                    .mock("GET", path2.as_str())
                    .with_status(200)
                    .expect(1)
                    .with_chunked_body(|w| {
                        std::thread::sleep(RESPONSE_LATENCY);
                        w.write_all(FASTIFY_ERROR_TARBALL)
                    })
                    .create_async()
                    .await;

                // Leak everything spawned tasks need to borrow. The test
                // is single-shot so we don't bother reclaiming.
                let (_store_keep, store_path) = tempdir_with_leaked_path();
                let client: &'static ThrottledClient =
                    Box::leak(Box::new(ThrottledClient::default()));
                let pkg_integrity: &'static Integrity =
                    Box::leak(Box::new(integrity(FASTIFY_ERROR_INTEGRITY)));
                let url1: &'static str = Box::leak(url1.into_boxed_str());
                let url2: &'static str = Box::leak(url2.into_boxed_str());

                let make_dts = |url: &'static str| DownloadTarballToStore {
                    http_client: client,
                    store_dir: store_path,
                    store_index: None,
                    store_index_writer: None,
                    verify_store_integrity: true,
                    package_integrity: pkg_integrity,
                    package_unpacked_size: None,
                    package_url: url,
                    package_id: "fastify-error@3.3.0",
                    prefetched_cas_paths: None,
                    retry_opts: RetryOpts { retries: 0, ..RetryOpts::default() },
                };

                // Spawn each task and yield once before the next so the
                // single worker drains the just-spawned task to its first
                // suspension point. With one worker, `yield_now` is a
                // deterministic ordering primitive (FIFO local queue):
                // A reaches `run_without_mem_cache`'s HTTP await, B
                // reaches the if-let branch's `notified().await` (with
                // the bug, holding the DashMap shard guard), and only
                // then is C polled — its else branch's
                // `mem_cache.insert` is what blocks the worker pre-fix.
                let task_a = tokio::spawn(make_dts(url1).run_with_mem_cache(mem_cache));
                tokio::task::yield_now().await;
                let task_b = tokio::spawn(make_dts(url1).run_with_mem_cache(mem_cache));
                tokio::task::yield_now().await;
                let task_c = tokio::spawn(make_dts(url2).run_with_mem_cache(mem_cache));

                task_a.await.expect("task A panicked").expect("task A failed");
                task_b.await.expect("task B panicked").expect("task B failed");
                task_c.await.expect("task C panicked").expect("task C failed");

                // Confirm each tarball endpoint was actually hit; without
                // these the test would pass vacuously if `run_with_mem_cache`
                // ever short-circuits before the network call.
                slow1.assert_async().await;
                slow2.assert_async().await;
            });

            // Reaching here means the runtime drained all three tasks —
            // i.e. no deadlock.
            let _ = tx.send(());
        })
        .expect("spawn regression-test thread");

    rx.recv_timeout(TEST_TIMEOUT).expect(
        "run_with_mem_cache deadlocked on DashMap shard contention; \
         single-worker runtime did not finish within the timeout",
    );
}

/// `retries: 0` (the value the existing fall-through tests use)
/// must produce exactly one network attempt — no extra request,
/// no backoff sleep. Guards against a future refactor that
/// off-by-ones the loop and turns `retries: 0` into "1 retry".
#[tokio::test]
async fn zero_retries_makes_a_single_attempt() {
    let (store_dir_keep, store_path) = tempdir_with_leaked_path();
    let mut server = mockito::Server::new_async().await;
    let mock = server.mock("GET", "/pkg.tgz").with_status(500).expect(1).create_async().await;

    let url = format!("{}/pkg.tgz", server.url());
    let client = ThrottledClient::default();
    let pkg_integrity = integrity(FASTIFY_ERROR_INTEGRITY);
    let opts = RetryOpts { retries: 0, ..fast_retry_opts() };

    fetch_and_extract_with_retry(&client, &url, &pkg_integrity, None, store_path, opts)
        .await
        .expect_err("retries=0 must surface the first failure");
    mock.assert_async().await;
    drop(store_dir_keep);
}
