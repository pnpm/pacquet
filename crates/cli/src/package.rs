use crate::package_import::{install_virtdir_by_snapshot, ImportMethodImpl};
use crate::package_manager::PackageManagerError;
use pacquet_lockfile::{DependencyPath, LockfileResolution, PackageSnapshot, PkgNameVerPeer};
use pacquet_npmrc::Npmrc;
use pacquet_registry::{Package, PackageVersion};
use pacquet_tarball::{download_tarball_to_store, Cache};
use reqwest::Client;
use std::path::Path;

/// This function execute the following and returns the package
/// - retrieves the package from the registry
/// - extracts the tarball to global store directory (~/Library/../pacquet)
/// - links global store directory to virtual dir (node_modules/.pacquet/..)
///
/// symlink_path will be appended by the name of the package. Therefore,
/// it should be resolved into the node_modules folder of a subdependency such as
/// `node_modules/.pacquet/fastify@1.0.0/node_modules`.
pub async fn install_package_from_registry(
    tarball_cache: &Cache,
    config: &'static Npmrc,
    http_client: &Client,
    name: &str,
    version_range: &str,
    symlink_path: &Path,
) -> Result<PackageVersion, PackageManagerError> {
    let package = Package::fetch_from_registry(name, http_client, &config.registry).await?;
    let package_version = package.pinned_version(version_range).unwrap();
    internal_fetch(tarball_cache, http_client, package_version, config, symlink_path).await?;
    Ok(package_version.to_owned())
}

pub async fn fetch_package_version_directly(
    tarball_cache: &Cache,
    config: &'static Npmrc,
    http_client: &Client,
    name: &str,
    version: &str,
    symlink_path: &Path,
) -> Result<PackageVersion, PackageManagerError> {
    let package_version =
        PackageVersion::fetch_from_registry(name, version, http_client, &config.registry).await?;
    internal_fetch(tarball_cache, http_client, &package_version, config, symlink_path).await?;
    Ok(package_version.to_owned())
}

async fn internal_fetch(
    tarball_cache: &Cache,
    http_client: &Client,
    package_version: &PackageVersion,
    config: &'static Npmrc,
    symlink_path: &Path,
) -> Result<(), PackageManagerError> {
    let store_folder_name = package_version.to_virtual_store_name();

    // TODO: skip when it already exists in store?
    let cas_paths = download_tarball_to_store(
        tarball_cache,
        http_client,
        &config.store_dir,
        package_version.dist.integrity.as_ref().expect("has integrity field"),
        package_version.dist.unpacked_size,
        package_version.as_tarball_url(),
    )
    .await?;

    let save_path = config
        .virtual_store_dir
        .join(store_folder_name)
        .join("node_modules")
        .join(&package_version.name);

    config.package_import_method.import(
        &cas_paths,
        &save_path,
        &symlink_path.join(&package_version.name),
    )?;

    Ok(())
}

#[allow(unused)] // for now
pub async fn install_single_package_to_virtual_store(
    tarball_cache: &Cache,
    http_client: &Client,
    config: &'static Npmrc,
    dependency_path: &DependencyPath,
    package_snapshot: &PackageSnapshot,
    virtual_store_dir: &Path,
) -> Result<(), PackageManagerError> {
    let PackageSnapshot { resolution, .. } = package_snapshot;
    let LockfileResolution::Registry(registry_resolution) = resolution else {
        panic!("Only TarballResolution is supported at the moment, but {dependency_path} requires {resolution:?}");
    };

    let DependencyPath { custom_registry, package_specifier } = dependency_path;
    let registry = custom_registry.as_ref().unwrap_or(&config.registry);
    let PkgNameVerPeer { name, suffix: version } = package_specifier;
    let package_version =
        PackageVersion::fetch_from_registry(name, version, http_client, registry).await?;

    let lockfile_integrity = registry_resolution.integrity.as_str();
    let remote_integrity = package_version.dist.integrity.as_deref().expect("has integrity field");
    if lockfile_integrity != remote_integrity {
        // TODO: convert this to a proper error variant in PackageManagerError
        panic!("Mismatch integrity for {dependency_path}: Expecting {lockfile_integrity}, but received {remote_integrity}");
    }

    let lockfile_name = package_specifier.name.as_str();
    let remote_name = package_version.name.as_str();
    if lockfile_name != remote_name {
        // TODO: convert this to a proper error variant in PackageManagerError
        // TODO: or may be handle this gracefully somehow?
        panic!("Mismatch name for {dependency_path}: Expecting {lockfile_name}, but received {remote_name}");
    }

    // TODO: skip when already exists in store?
    let cas_paths = download_tarball_to_store(
        tarball_cache,
        http_client,
        &config.store_dir,
        lockfile_integrity,
        package_version.dist.unpacked_size,
        package_version.as_tarball_url(),
    )
    .await?;

    install_virtdir_by_snapshot(
        dependency_path,
        &config.virtual_store_dir,
        &cas_paths,
        config.package_import_method,
        package_snapshot,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::package::install_package_from_registry;
    use node_semver::Version;
    use pacquet_npmrc::Npmrc;
    use pipe_trait::Pipe;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn create_config(store_dir: &Path, modules_dir: &Path, virtual_store_dir: &Path) -> Npmrc {
        Npmrc {
            hoist: false,
            hoist_pattern: vec![],
            public_hoist_pattern: vec![],
            shamefully_hoist: false,
            store_dir: store_dir.to_path_buf(),
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
        let http_client = reqwest::Client::new();
        let symlink_path = tempdir().unwrap();
        let package = install_package_from_registry(
            &Default::default(),
            config,
            &http_client,
            "fast-querystring",
            "1.0.0",
            symlink_path.path(),
        )
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
        assert!(virtual_store_path.is_dir());

        // Make sure the symlink is resolving to the correct path
        assert_eq!(
            fs::read_link(symlink_path.path().join(&package.name)).unwrap(),
            virtual_store_path
        );
    }
}
