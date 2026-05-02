use super::InstallPackageFromRegistry;
use node_semver::Version;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_reporter::SilentReporter;
use pacquet_store_dir::{SharedVerifiedFilesCache, StoreDir};
use pipe_trait::Pipe;
use pretty_assertions::assert_eq;
use std::{fs, path::Path, sync::atomic::AtomicU8};
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
