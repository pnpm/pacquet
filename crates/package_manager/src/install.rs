use crate::{InstallFrozenLockfile, InstallWithoutLockfile};
use pacquet_lockfile::Lockfile;
use pacquet_npmrc::Npmrc;
use pacquet_package_json::{DependencyGroup, PackageJson};
use pacquet_tarball::Cache;
use reqwest::Client;

/// This subroutine does everything `pacquet install` is supposed to do.
#[must_use]
pub struct Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Shared cache that store downloaded tarballs.
    pub tarball_cache: &'a Cache,
    /// HTTP client to make HTTP requests.
    pub http_client: &'a Client,
    /// Configuration read from `.npmrc`.
    pub config: &'static Npmrc,
    /// Data from the `package.json` file.
    pub package_json: &'a PackageJson,
    /// Data from the `pnpm-lock.yaml` file.
    pub lockfile: Option<&'a Lockfile>,
    /// List of [`DependencyGroup`]s.
    pub dependency_groups: DependencyGroupList,
    /// Whether `--frozen-lockfile` is specified.
    pub frozen_lockfile: bool,
}

impl<'a, DependencyGroupList> Install<'a, DependencyGroupList>
where
    DependencyGroupList: IntoIterator<Item = DependencyGroup>,
{
    /// Execute the subroutine.
    pub async fn run(self) {
        let Install {
            tarball_cache,
            http_client,
            config,
            package_json,
            lockfile,
            dependency_groups,
            frozen_lockfile,
        } = self;

        tracing::info!(target: "pacquet::install", "Start all");

        match (config.lockfile, frozen_lockfile, lockfile) {
            (false, _, _) => {
                InstallWithoutLockfile {
                    tarball_cache,
                    http_client,
                    config,
                    package_json,
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
                    tarball_cache,
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
    use pacquet_package_json::{DependencyGroup, PackageJson};
    use std::{env, io};
    use tempfile::tempdir;

    // Helper function to check if a path is a symlink or junction
    fn is_symlink_or_junction(path: std::path::PathBuf) -> io::Result<bool> {
        #[cfg(windows)]
        return junction::exists(&path);

        #[cfg(not(windows))]
        return Ok(path.is_symlink());
    }

    pub fn get_all_folders(root: &std::path::Path) -> Vec<String> {
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(root) {
            let entry = entry.unwrap();
            let entry_path = entry.path();
            if entry.file_type().is_dir() || entry.file_type().is_symlink() {
                // We need this mutation to ensure that both Unix and Windows paths resolves the same.
                // TODO: Find a better way to do this?
                let simple_path = entry_path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_str().expect("invalid UTF-8"))
                    .collect::<Vec<_>>()
                    .join("/");

                if !simple_path.is_empty() {
                    files.push(simple_path);
                }
            }
        }
        files.sort();
        files
    }

    #[tokio::test]
    pub async fn should_install_dependencies() {
        let dir = tempdir().unwrap();
        let store_dir = dir.path().join("pacquet-store");
        let project_root = dir.path().join("project");
        let modules_dir = project_root.join("node_modules"); // TODO: we shouldn't have to define this
        let virtual_store_dir = modules_dir.join(".pacquet"); // TODO: we shouldn't have to define this

        let package_json_path = dir.path().join("package.json");
        let mut package_json = PackageJson::create_if_needed(package_json_path.clone()).unwrap();

        package_json.add_dependency("is-odd", "3.0.1", DependencyGroup::Default).unwrap();
        package_json
            .add_dependency("fast-decode-uri-component", "1.0.1", DependencyGroup::Dev)
            .unwrap();

        package_json.save().unwrap();

        let mut config = Npmrc::new();
        config.store_dir = store_dir.to_path_buf();
        config.modules_dir = modules_dir.to_path_buf();
        config.virtual_store_dir = virtual_store_dir.to_path_buf();
        let config = config.leak();

        Install {
            tarball_cache: &Default::default(),
            http_client: &Default::default(),
            config,
            package_json: &package_json,
            lockfile: None,
            dependency_groups: [
                DependencyGroup::Default,
                DependencyGroup::Dev,
                DependencyGroup::Optional,
            ],
            frozen_lockfile: false,
        }
        .run()
        .await;

        // Make sure the package is installed
        assert!(is_symlink_or_junction(project_root.join("node_modules/is-odd")).unwrap());
        assert!(project_root.join("node_modules/.pacquet/is-odd@3.0.1").exists());
        // Make sure it installs direct dependencies
        assert!(!project_root.join("node_modules/is-number").exists());
        assert!(project_root.join("node_modules/.pacquet/is-number@6.0.0").exists());
        // Make sure we install dev-dependencies as well
        assert!(is_symlink_or_junction(
            project_root.join("node_modules/fast-decode-uri-component")
        )
        .unwrap());
        assert!(project_root
            .join("node_modules/.pacquet/fast-decode-uri-component@1.0.1")
            .is_dir());

        insta::assert_debug_snapshot!(get_all_folders(&project_root));
    }
}
