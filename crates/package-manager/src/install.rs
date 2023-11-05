use crate::{InstallFrozenLockfile, InstallWithoutLockfile};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_manifest::{DependencyGroup, PackageManifest};
use pacquet_tarball::MemCache;
use reqwest::Client;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    pub tarball_mem_cache: &'a MemCache,
    pub http_client: &'a Client,
    pub config: &'static Npmrc,
    pub manifest: &'a PackageManifest,
    pub lockfile: Option<&'a Lockfile>,
    pub dependency_groups: DependencyGroupList,
    pub frozen_lockfile: bool,
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) {
        let Install {
            tarball_mem_cache,
            http_client,
            config,
            manifest,
            lockfile,
            dependency_groups,
            frozen_lockfile,
        } = self;

        tracing::info!(target: "pacquet::install", "Start all");

        match (config.lockfile, frozen_lockfile, lockfile) {
            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_mem_cache,
                    http_client,
                    config,
                    manifest,
                    dependency_groups,
                }
                .run()
                .await;
            }
            (true, false, Some(_)) | (true, false, None) | (true, true, None) => {
                unimplemented!();
            }
            (true, true, Some(lockfile)) => {
                let Lockfile { lockfile_version, project_snapshot, packages, .. } = lockfile;
                assert_eq!(lockfile_version.major, 6); // compatibility check already happens at serde, but this still helps preventing programmer mistakes.

                InstallFrozenLockfile {
                    http_client,
                    config,
                    project_snapshot,
                    packages: packages.as_ref(),
                    dependency_groups,
                }
                .run()
                .await;
            }
        }

        tracing::info!(target: "pacquet::install", "Complete all");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pacquet_npmrc::Npmrc;
    use pacquet_package_manifest::{DependencyGroup, PackageManifest};
    use pacquet_testing_utils::fs::{get_all_folders, is_symlink_or_junction};
    use std::env;
    use tempfile::tempdir;

    #[tokio::test]
    async fn should_install_dependencies() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules"); // TODO: we shouldn't have to define this
        let virtual_store_dir = modules_dir.join(".pacquet"); // TODO: we shouldn't have to define this

        let manifest_path = dir.path().join("package.json");
        let mut manifest = PackageManifest::create_if_needed(manifest_path.clone()).unwrap();

        manifest.add_dependency("is-odd", "3.0.1", DependencyGroup::Prod).unwrap();
        manifest
            .add_dependency("fast-decode-uri-component", "1.0.1", DependencyGroup::Dev)
            .unwrap();

        manifest.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.into();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        let config = config.leak();

        Install {
            tarball_mem_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            manifest: &manifest,
            lockfile: None,
            dependency_groups: [
                DependencyGroup::Prod,
                DependencyGroup::Dev,
                DependencyGroup::Optional,
            ],
            frozen_lockfile: false,
        }
        .run()
        .await;

        // Make sure the package is installed
        assert!(is_symlink_or_junction(&project_root.join("node_modules/is-odd")).unwrap());
        assert!(project_root.join("node_modules/.pacquet/is-odd@3.0.1").exists());
        // Make sure it installs direct dependencies
        assert!(!project_root.join("node_modules/is-number").exists());
        assert!(project_root.join("node_modules/.pacquet/is-number@6.0.0").exists());
        // Make sure we install dev-dependencies as well
        assert!(is_symlink_or_junction(
            &project_root.join("node_modules/fast-decode-uri-component")
        )
        .unwrap());
        assert!(project_root
            .join("node_modules/.pacquet/fast-decode-uri-component@1.0.1")
            .is_dir());

        insta::assert_debug_snapshot!(get_all_folders(&project_root));
    }
}
