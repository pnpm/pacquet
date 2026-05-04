use super::InstallPackageFromRegistry;
use node_semver::Version;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_registry_mock::AutoMockInstance;
use pacquet_reporter::{LogEvent, ProgressMessage, Reporter, SilentReporter};
use pacquet_store_dir::{SharedVerifiedFilesCache, StoreDir};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::{
    fs,
    path::Path,
    sync::{Mutex, atomic::AtomicU8},
};
use tempfile::tempdir;

fn create_config(store_dir: &Path, modules_dir: &Path, virtual_store_dir: &Path) -> Npmrc {
    Npmrc {
        hoist: false,
        hoist_pattern: vec![],
        public_hoist_pattern: vec![],
        shamefully_hoist: false,
        store_dir: StoreDir::new(store_dir),
        modules_dir: modules_dir.to_path_buf(),
        node_linker: Default::default(),
        symlink: false,
        virtual_store_dir: virtual_store_dir.to_path_buf(),
        package_import_method: Default::default(),
        modules_cache_max_age: 0,
        lockfile: false,
        prefer_frozen_lockfile: false,
        lockfile_include_tarball_url: false,
        registry: "https://registry.npmjs.com/".to_string(),
        auto_install_peers: false,
        dedupe_peer_dependents: false,
        strict_peer_dependencies: false,
        resolve_peers_from_workspace_root: false,
        verify_store_integrity: true,
        fetch_retries: 2,
        fetch_retry_factor: 10,
        fetch_retry_mintimeout: 10_000,
        fetch_retry_maxtimeout: 60_000,
        auth_headers: Default::default(),
    }
}

#[tokio::test]
pub async fn should_find_package_version_from_registry() {
    let store_dir = tempdir().unwrap();
    let modules_dir = tempdir().unwrap();
    let virtual_store_dir = tempdir().unwrap();
    let config: &'static Npmrc =
        create_config(store_dir.path(), modules_dir.path(), virtual_store_dir.path())
            .pipe(Box::new)
            .pipe(Box::leak);
    let http_client = ThrottledClient::new_for_installs();
    let verified_files_cache = SharedVerifiedFilesCache::default();
    let logged_methods = AtomicU8::new(0);
    let package = InstallPackageFromRegistry {
        tarball_mem_cache: &Default::default(),
        config,
        http_client: &http_client,
        store_index: None,
        store_index_writer: None,
        verified_files_cache: &verified_files_cache,
        logged_methods: &logged_methods,
        requester: "",
        name: "fast-querystring",
        version_range: "1.0.0",
        node_modules_dir: modules_dir.path(),
    }
    .run::<SilentReporter>()
    .await
    .unwrap();

    assert_eq!(package.name, "fast-querystring");
    assert_eq!(
        package.version,
        Version { major: 1, minor: 0, patch: 0, build: vec![], pre_release: vec![] }
    );

    let virtual_store_path = virtual_store_dir
        .path()
        .join(package.to_virtual_store_name())
        .join("node_modules")
        .join(&package.name);
    eprintln!(
        "virtual_store_path={virtual_store_path:?} exists={} is_dir={}",
        virtual_store_path.exists(),
        virtual_store_path.is_dir(),
    );
    assert!(virtual_store_path.is_dir());

    // Make sure the symlink is resolving to the correct path
    assert_eq!(fs::read_link(modules_dir.path().join(&package.name)).unwrap(), virtual_store_path);
}

/// `InstallPackageFromRegistry::run` (the no-lockfile path) emits
/// the `pnpm:progress` per-package sequence: `resolved` before the
/// tarball download, then `fetched` (or `found_in_store` on a cache
/// hit) from inside `DownloadTarballToStore`, then `imported` after
/// `create_cas_files` returns Ok. Pin the order with a recording
/// reporter — a regression in either the sequence or the
/// `package_id`/`requester` payload would currently slip through
/// since the tarball-side and frozen-lockfile-side tests don't
/// exercise this code path.
///
/// Uses `AutoMockInstance` (the workspace's local mock registry) so
/// the test isn't network-dependent — same pattern as
/// `install::tests::should_install_dependencies`.
#[tokio::test]
async fn no_lockfile_install_emits_progress_sequence() {
    static EVENTS: Mutex<Vec<LogEvent>> = Mutex::new(Vec::new());
    EVENTS.lock().unwrap().clear();

    struct RecordingReporter;
    impl Reporter for RecordingReporter {
        fn emit(event: &LogEvent) {
            EVENTS.lock().unwrap().push(event.clone());
        }
    }

    let mock_instance = AutoMockInstance::load_or_init();

    let store_dir = tempdir().unwrap();
    let modules_dir = tempdir().unwrap();
    let virtual_store_dir = tempdir().unwrap();

    let mut config = create_config(store_dir.path(), modules_dir.path(), virtual_store_dir.path());
    config.registry = mock_instance.url();
    let config: &'static Npmrc = config.pipe(Box::new).pipe(Box::leak);

    let http_client = ThrottledClient::new_for_installs();
    let verified_files_cache = SharedVerifiedFilesCache::default();
    let logged_methods = AtomicU8::new(0);

    let _package = InstallPackageFromRegistry {
        tarball_mem_cache: &Default::default(),
        config,
        http_client: &http_client,
        store_index: None,
        store_index_writer: None,
        verified_files_cache: &verified_files_cache,
        logged_methods: &logged_methods,
        requester: "/proj",
        name: "@pnpm.e2e/hello-world-js-bin",
        version_range: "1.0.0",
        node_modules_dir: modules_dir.path(),
    }
    .run::<RecordingReporter>()
    .await
    .expect("install should succeed against the mock registry");

    let progress: Vec<ProgressMessage> = EVENTS
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            LogEvent::Progress(log) => Some(log.message.clone()),
            _ => None,
        })
        .collect();

    // Order: resolved → fetched (or found_in_store on a warm rerun)
    // → imported. The mock store is a tempdir, so the first install
    // always goes through the network path → `Fetched`. Pin the
    // shape so a future re-ordering breaks the test.
    let kinds: Vec<&'static str> = progress
        .iter()
        .map(|m| match m {
            ProgressMessage::Resolved { .. } => "resolved",
            ProgressMessage::Fetched { .. } => "fetched",
            ProgressMessage::FoundInStore { .. } => "found_in_store",
            ProgressMessage::Imported { .. } => "imported",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["resolved", "fetched", "imported"],
        "unexpected progress sequence: {progress:?}",
    );

    // Pin the (`package_id`, `requester`) on the resolved event —
    // the install layer threads `requester` here as the install
    // root; `package_id` is `{name}@{version}` once the version is
    // resolved.
    match &progress[0] {
        ProgressMessage::Resolved { package_id, requester } => {
            assert_eq!(package_id, "@pnpm.e2e/hello-world-js-bin@1.0.0");
            assert_eq!(requester, "/proj");
        }
        other => panic!("first event must be Resolved; got {other:?}"),
    }

    drop((store_dir, modules_dir, virtual_store_dir, mock_instance));
}
