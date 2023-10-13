use crate::InstallPackageFromRegistry;
use async_recursion::async_recursion;
use futures_util::future;
use node_semver::Version;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_registry::PackageVersion;
use pacquet_tarball::Cache;
use pipe_trait::Pipe;
use reqwest::Client;

/// This subroutine install packages from a `package.json` without reading or writing a lockfile.
#[must_use]
pub struct InstallWithoutLockfile<'a, DependencyGroupList> {
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Configuration read from `.npmrc`.
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub package_json: &'a PackageJson,
    /// List of [`DependencyGroup`]s.
    pub dependency_groups: DependencyGroupList,
}

impl<'a, DependencyGroupList> InstallWithoutLockfile<'a, DependencyGroupList> {
    /// Execute the subroutine.
    pub async fn run(self)
    where
        DependencyGroupList: IntoIterator<Item = DependencyGroup>,
    {
        let InstallWithoutLockfile {
            tarball_cache,
            http_client,
            config,
            package_json,
            dependency_groups,
        } = self;

        let _: Vec<()> = package_json
            .dependencies(dependency_groups.into_iter())
            .map(|(name, version_range)| async move {
                let dependency = InstallPackageFromRegistry {
                    tarball_cache,
                    http_client,
                    config,
                    node_modules_dir: &config.modules_dir,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .unwrap();

                InstallWithoutLockfile {
                    tarball_cache,
                    http_client,
                    config,
                    package_json,
                    dependency_groups: (),
                }
                .install_dependencies_from_registry(&dependency)
                .await;
            })
            .pipe(future::join_all)
            .await;
    }
}

impl<'a> InstallWithoutLockfile<'a, ()> {
    /// Install dependencies of a dependency.
    #[async_recursion]
    async fn install_dependencies_from_registry(&self, package: &PackageVersion) {
        let InstallWithoutLockfile { tarball_cache, http_client, config, .. } = self;

        let node_modules_path = self
            .config
            .virtual_store_dir
            .join(package.to_virtual_store_name())
            .join("node_modules");

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Start subset");

        package
            .dependencies(self.config.auto_install_peers)
            .map(|(name, version_range)| async {
                let dependency = InstallPackageFromRegistry {
                    tarball_cache,
                    http_client,
                    config,
                    node_modules_dir: &node_modules_path,
                    name,
                    version_range,
                }
                .run::<Version>()
                .await
                .unwrap(); // TODO: proper error propagation
                self.install_dependencies_from_registry(&dependency).await;
            })
            .pipe(future::join_all)
            .await;

        tracing::info!(target: "pacquet::install", node_modules = ?node_modules_path, "Complete subset");
    }
}
