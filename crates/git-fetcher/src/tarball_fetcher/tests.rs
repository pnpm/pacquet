use super::GitHostedTarballFetcher;
use crate::error::{GitFetcherError, PreparePackageError};
use pacquet_executor::ScriptsPrependNodePath;
use pacquet_reporter::SilentReporter;
use pacquet_store_dir::StoreDir;
use std::{collections::HashMap, fs, path::PathBuf};
use tempfile::tempdir;

fn deny_all_builds<'a>() -> &'a (dyn Fn(&str, &str) -> bool + Send + Sync) {
    &|_, _| false
}

/// Build the `cas_paths` map the dispatcher would hand the fetcher
/// after `DownloadTarballToStore` finishes: a fresh `StoreDir`, a few
/// files written via `write_cas_file`, and a `path → cas_path` map.
fn write_to_cas(store_dir: &StoreDir, files: &[(&str, &[u8], bool)]) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    for &(rel, bytes, executable) in files {
        let (cas_path, _hash) = store_dir.write_cas_file(bytes, executable).unwrap();
        out.insert(rel.to_string(), cas_path);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn passes_through_package_without_scripts() {
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let cas_paths = write_to_cas(
        &store_dir,
        &[
            ("package.json", br#"{"name":"x","version":"1.0.0","main":"index.js"}"#, false),
            ("index.js", b"module.exports = 42;\n", false),
            // A README that the packlist's always-included rule
            // should preserve regardless of the (absent) `files`
            // field.
            ("README.md", b"# x\n", false),
        ],
    );

    let received = GitHostedTarballFetcher {
        cas_paths: cas_paths.clone(),
        path: None,
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "x@1.0.0",
        requester: "/test",
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    assert!(!received.built, "no `prepare` script → not built");
    assert!(received.cas_paths.contains_key("package.json"));
    assert!(received.cas_paths.contains_key("index.js"));
    assert!(received.cas_paths.contains_key("README.md"));

    // Hash-dedup: re-importing the same bytes lands at the same CAS
    // path, so the new map's CAS entries point at the same files we
    // wrote up front.
    for (rel, original) in &cas_paths {
        assert_eq!(received.cas_paths.get(rel), Some(original), "deterministic CAS path for {rel}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_files_outside_files_field() {
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let cas_paths = write_to_cas(
        &store_dir,
        &[
            ("package.json", br#"{"name":"x","version":"1.0.0","files":["dist/**"]}"#, false),
            ("dist/index.js", b"// built\n", false),
            ("src/index.ts", b"// source\n", false),
            ("test/foo.test.js", b"// test\n", false),
        ],
    );

    let received = GitHostedTarballFetcher {
        cas_paths,
        path: None,
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "x@1.0.0",
        requester: "/test",
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    let keys: Vec<&str> = received.cas_paths.keys().map(String::as_str).collect();
    assert!(keys.contains(&"dist/index.js"));
    assert!(keys.contains(&"package.json"), "package.json always included");
    assert!(!keys.contains(&"src/index.ts"), "src excluded by files field");
    assert!(!keys.contains(&"test/foo.test.js"), "test excluded by files field");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_build_when_not_allowed() {
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let cas_paths = write_to_cas(
        &store_dir,
        &[
            (
                "package.json",
                br#"{"name":"naughty","version":"2.0.0","main":"index.js","scripts":{"prepare":"tsc"}}"#,
                false,
            ),
            ("index.js", b"module.exports = 1;\n", false),
        ],
    );

    let err = GitHostedTarballFetcher {
        cas_paths,
        path: None,
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "naughty@2.0.0",
        requester: "/test",
    }
    .run::<SilentReporter>()
    .await
    .unwrap_err();

    match err {
        GitFetcherError::Prepare(PreparePackageError::NotAllowed { name, version }) => {
            assert_eq!(name, "naughty");
            assert_eq!(version, "2.0.0");
        }
        other => panic!("expected Prepare::NotAllowed, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn path_field_packs_only_subdirectory() {
    // Git-hosted tarballs from monorepos pin a `path` to point at the
    // sub-package they actually publish. The fetcher must run
    // `preparePackage` + `packlist` inside that sub-dir so the
    // resulting `cas_paths` only contain that package's files.
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let cas_paths = write_to_cas(
        &store_dir,
        &[
            // Monorepo root manifest — not the published package.
            ("package.json", br#"{"name":"monorepo","version":"0.0.0","private":true}"#, false),
            // The sub-package we're packing.
            (
                "packages/sub/package.json",
                br#"{"name":"sub","version":"1.0.0","main":"index.js"}"#,
                false,
            ),
            ("packages/sub/index.js", b"module.exports = 1;\n", false),
            ("packages/sub/README.md", b"# sub\n", false),
            // A sibling package that must NOT end up in the result.
            ("packages/other/package.json", br#"{"name":"other","version":"1.0.0"}"#, false),
            ("packages/other/index.js", b"// other\n", false),
        ],
    );

    let received = GitHostedTarballFetcher {
        cas_paths,
        path: Some("packages/sub"),
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "sub@1.0.0",
        requester: "/test",
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    let keys: Vec<&str> = received.cas_paths.keys().map(String::as_str).collect();
    // The fetcher packlists relative to `pkg_dir` (which is
    // `<tmp>/packages/sub`), so the returned keys are *also* relative
    // to that sub-dir — never carrying the `packages/sub/` prefix.
    assert!(keys.contains(&"package.json"), "sub-dir manifest must be included");
    assert!(keys.contains(&"index.js"), "sub-dir main must be included");
    assert!(keys.contains(&"README.md"), "always-included file must be included");
    assert!(
        !keys.iter().any(|k| k.contains("other")),
        "sibling-package files must not appear in {keys:?}",
    );
    assert!(
        !keys.iter().any(|k| k.contains("packages/")),
        "keys are relative to the sub-dir, not the monorepo root: {keys:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn materialized_temp_dir_does_not_corrupt_cas() {
    // Regression: when the prepare phase modifies a working-tree
    // file, the CAS entry it was sourced from must remain unchanged.
    // We exercise the materialization path explicitly: a fresh
    // working tree (made via `fs::copy`) should have a different
    // inode than the CAS entry on POSIX.
    let store_root = tempdir().unwrap();
    let store_dir = StoreDir::from(store_root.path().to_path_buf());

    let cas_paths =
        write_to_cas(&store_dir, &[("package.json", br#"{"name":"x","version":"1.0.0"}"#, false)]);
    let original_cas_path = cas_paths["package.json"].clone();
    let cas_bytes_before = fs::read(&original_cas_path).unwrap();

    let _ = GitHostedTarballFetcher {
        cas_paths,
        path: None,
        allow_build: deny_all_builds(),
        ignore_scripts: false,
        unsafe_perm: true,
        user_agent: None,
        scripts_prepend_node_path: ScriptsPrependNodePath::Never,
        script_shell: None,
        node_execpath: None,
        npm_execpath: None,
        store_dir: &store_dir,
        package_id: "x@1.0.0",
        requester: "/test",
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    let cas_bytes_after = fs::read(&original_cas_path).unwrap();
    assert_eq!(
        cas_bytes_before, cas_bytes_after,
        "fetcher must not mutate CAS entries it sourced from",
    );
}
