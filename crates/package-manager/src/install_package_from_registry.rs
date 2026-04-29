use crate::{
    CreateCasFilesError, SymlinkPackageError, create_cas_files,
    retry_config::retry_opts_from_config, symlink_package,
};
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_network::ThrottledClient;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::PackageManifest;
use pacquet_registry::{Package, PackageTag, PackageVersion, RegistryError};
use pacquet_store_dir::{SharedReadonlyStoreIndex, SharedVerifiedFilesCache, StoreIndexWriter};
use pacquet_tarball::{DownloadTarballToStore, MemCache, TarballError};
use std::{path::Path, sync::Arc};

/// This subroutine executes the following and returns the package
/// * Retrieves the package from the registry
/// * Extracts the tarball to global store directory (~/Library/../pacquet)
/// * Links global store directory to virtual dir (node_modules/.pacquet/..)
///
/// `name` is the manifest dependency key — the directory name the
/// package will be exposed as inside `node_modules`. For an npm-alias
/// entry (`"foo": "npm:bar@^1.0.0"`), `name` is the local alias (`foo`)
/// and the actual registry package name (`bar`) is parsed out of
/// `version_range` before the registry lookup.
///
/// `symlink_path` will be appended by `name`. Therefore, it should be
/// resolved into the node_modules folder of a subdependency such as
/// `node_modules/.pacquet/fastify@1.0.0/node_modules`.
#[must_use]
pub struct InstallPackageFromRegistry<'a> {
    pub tarball_mem_cache: &'a MemCache,
    pub http_client: &'a ThrottledClient,
    pub config: &'static Npmrc,
    pub store_index: Option<&'a SharedReadonlyStoreIndex>,
    pub store_index_writer: Option<&'a Arc<StoreIndexWriter>>,
    /// Install-scoped `verifiedFilesCache` shared across every
    /// per-package fetch. See `DownloadTarballToStore::verified_files_cache`
    /// for the rationale.
    pub verified_files_cache: &'a SharedVerifiedFilesCache,
    pub node_modules_dir: &'a Path,
    pub name: &'a str,
    pub version_range: &'a str,
}

/// Error type of [`InstallPackageFromRegistry`].
#[derive(Debug, Display, Error, Diagnostic)]
pub enum InstallPackageFromRegistryError {
    FetchFromRegistry(#[error(source)] RegistryError),
    DownloadTarballToStore(#[error(source)] TarballError),
    CreateCasFiles(#[error(source)] CreateCasFilesError),
    SymlinkPackage(#[error(source)] SymlinkPackageError),
}

impl<'a> InstallPackageFromRegistry<'a> {
    /// Execute the subroutine.
    pub async fn run(self) -> Result<PackageVersion, InstallPackageFromRegistryError> {
        let &InstallPackageFromRegistry { http_client, config, name, version_range, .. } = &self;

        // Strip any `npm:<name>@<range>` alias prefix before talking to
        // the registry. `name` (the manifest key) stays as the directory
        // name inside `node_modules`. Unversioned aliases (`npm:foo`) are
        // resolved to `"latest"` by `resolve_registry_dependency`.
        let (registry_name, version_range) =
            PackageManifest::resolve_registry_dependency(name, version_range);

        // Try parsing as a `PackageTag` first: this covers both the
        // `"latest"` tag (including unversioned `npm:` aliases) and
        // pinned versions like `"1.0.0"`. Semver ranges like `"^1.0.0"`
        // fail `PackageTag::from_str` and fall through to the range
        // resolution branch below.
        Ok(if let Ok(tag) = version_range.parse::<PackageTag>() {
            let package_version = PackageVersion::fetch_from_registry(
                registry_name,
                tag,
                http_client,
                &config.registry,
                &config.auth_headers,
            )
            .await
            .map_err(InstallPackageFromRegistryError::FetchFromRegistry)?;
            self.install_package_version(&package_version).await?;
            package_version
        } else {
            let package = Package::fetch_from_registry(
                registry_name,
                http_client,
                &config.registry,
                &config.auth_headers,
            )
            .await
            .map_err(InstallPackageFromRegistryError::FetchFromRegistry)?;
            let package_version = package.pinned_version(version_range).unwrap(); // TODO: propagate error for when no version satisfies range
            self.install_package_version(package_version).await?;
            package_version.clone()
        })
    }

    async fn install_package_version(
        self,
        package_version: &PackageVersion,
    ) -> Result<(), InstallPackageFromRegistryError> {
        let InstallPackageFromRegistry {
            tarball_mem_cache,
            http_client,
            config,
            store_index,
            store_index_writer,
            verified_files_cache,
            node_modules_dir,
            name,
            ..
        } = self;

        let store_folder_name = package_version.to_virtual_store_name();
        let package_id = format!("{0}@{1}", package_version.name, package_version.version);

        // TODO: skip when it already exists in store?
        let cas_paths = DownloadTarballToStore {
            http_client,
            store_dir: &config.store_dir,
            store_index: store_index.cloned(),
            store_index_writer: store_index_writer.cloned(),
            verify_store_integrity: config.verify_store_integrity,
            verified_files_cache: SharedVerifiedFilesCache::clone(verified_files_cache),
            package_integrity: package_version
                .dist
                .integrity
                .as_ref()
                .expect("has integrity field"),
            package_unpacked_size: package_version.dist.unpacked_size,
            package_url: package_version.as_tarball_url(),
            package_id: &package_id,
            prefetched_cas_paths: None,
            retry_opts: retry_opts_from_config(config),
            auth_headers: &config.auth_headers,
        }
        .run_with_mem_cache(tarball_mem_cache)
        .await
        .map_err(InstallPackageFromRegistryError::DownloadTarballToStore)?;

        // The virtual store always uses the registry-returned name
        // (`package_version.name`), so npm-alias entries share a single
        // virtual store directory with their non-aliased counterparts.
        // The exposed symlink under `node_modules/` uses the manifest
        // key (`name`) so both forms can coexist in the same parent.
        let save_path = config
            .virtual_store_dir
            .join(store_folder_name)
            .join("node_modules")
            .join(&package_version.name);

        let symlink_path = node_modules_dir.join(name);

        tracing::info!(target: "pacquet::import", ?save_path, ?symlink_path, "Import package");

        create_cas_files(config.package_import_method, &save_path, &cas_paths)
            .map_err(InstallPackageFromRegistryError::CreateCasFiles)?;

        symlink_package(&save_path, &symlink_path)
            .map_err(InstallPackageFromRegistryError::SymlinkPackage)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_semver::Version;
    use pacquet_npmrc::Npmrc;
    use pacquet_store_dir::StoreDir;
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
        let package = InstallPackageFromRegistry {
            tarball_mem_cache: &Default::default(),
            config,
            http_client: &http_client,
            store_index: None,
            store_index_writer: None,
            verified_files_cache: &verified_files_cache,
            name: "fast-querystring",
            version_range: "1.0.0",
            node_modules_dir: modules_dir.path(),
        }
        .run()
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
            fs::read_link(modules_dir.path().join(&package.name)).unwrap(),
            virtual_store_path
        );
    }
}
