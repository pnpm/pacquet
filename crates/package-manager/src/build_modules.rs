use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_executor::{LifecycleScriptError, RunPostinstallHooks, run_postinstall_hooks};
use pacquet_lockfile::{PackageKey, PackageMetadata, SnapshotEntry};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

/// Error from the build-modules step.
#[derive(Debug, Display, Error, Diagnostic)]
pub enum BuildModulesError {
    #[diagnostic(transparent)]
    LifecycleScript(#[error(source)] LifecycleScriptError),
}

/// Run lifecycle scripts for all packages that require a build.
///
/// Ports the core of `buildModules` from
/// `https://github.com/pnpm/pnpm/blob/7e91e4b35f/building/during-install/src/index.ts`.
///
/// This is a simplified implementation that runs scripts sequentially.
/// A future iteration should topologically sort the dep graph and run
/// scripts concurrently within each chunk.
pub struct BuildModules<'a> {
    pub virtual_store_dir: &'a Path,
    pub modules_dir: &'a Path,
    pub lockfile_dir: &'a Path,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
}

impl<'a> BuildModules<'a> {
    pub fn run(self) -> Result<(), BuildModulesError> {
        let BuildModules { virtual_store_dir, modules_dir, lockfile_dir, packages, snapshots } =
            self;

        let Some(snapshots) = snapshots else { return Ok(()) };
        let Some(packages) = packages else { return Ok(()) };

        let extra_env = HashMap::new();
        let extra_bin_paths: Vec<PathBuf> = vec![];

        for snapshot_key in snapshots.keys() {
            let metadata_key = snapshot_key.without_peer();
            let Some(metadata) = packages.get(&metadata_key) else { continue };

            if metadata.requires_build != Some(true) {
                continue;
            }

            let pkg_dir = virtual_store_dir_for_key(virtual_store_dir, snapshot_key);
            if !pkg_dir.exists() {
                continue;
            }

            tracing::info!(
                target: "pacquet::build",
                package = %snapshot_key,
                dir = %pkg_dir.display(),
                "running lifecycle scripts",
            );

            run_postinstall_hooks(RunPostinstallHooks {
                dep_path: &snapshot_key.to_string(),
                pkg_root: &pkg_dir,
                root_modules_dir: modules_dir,
                init_cwd: lockfile_dir,
                extra_bin_paths: &extra_bin_paths,
                extra_env: &extra_env,
                optional: false,
            })
            .map_err(BuildModulesError::LifecycleScript)?;
        }

        Ok(())
    }
}

/// Run lifecycle scripts by scanning the virtual store directory.
///
/// Used by the non-frozen install path which does not have lockfile
/// metadata to determine `requires_build`. Instead, each package's
/// `package.json` is checked for lifecycle scripts directly.
pub struct BuildModulesByScanning<'a> {
    pub virtual_store_dir: &'a Path,
    pub modules_dir: &'a Path,
    pub lockfile_dir: &'a Path,
}

impl<'a> BuildModulesByScanning<'a> {
    pub fn run(self) -> Result<(), BuildModulesError> {
        let BuildModulesByScanning { virtual_store_dir, modules_dir, lockfile_dir } = self;

        if !virtual_store_dir.exists() {
            return Ok(());
        }

        let extra_env = HashMap::new();
        let extra_bin_paths: Vec<PathBuf> = vec![];

        let entries = match fs::read_dir(virtual_store_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };

        for entry in entries.flatten() {
            let store_entry = entry.path();
            let node_modules = store_entry.join("node_modules");
            if !node_modules.is_dir() {
                continue;
            }

            for pkg_entry in walk_package_dirs(&node_modules) {
                if !has_lifecycle_scripts(&pkg_entry) {
                    continue;
                }

                let dep_path = store_entry
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                tracing::info!(
                    target: "pacquet::build",
                    dep_path,
                    dir = %pkg_entry.display(),
                    "running lifecycle scripts (scan)",
                );

                match run_postinstall_hooks(RunPostinstallHooks {
                    dep_path: &dep_path,
                    pkg_root: &pkg_entry,
                    root_modules_dir: modules_dir,
                    init_cwd: lockfile_dir,
                    extra_bin_paths: &extra_bin_paths,
                    extra_env: &extra_env,
                    optional: false,
                }) {
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(
                            target: "pacquet::build",
                            dep_path,
                            error = %err,
                            "lifecycle script failed; continuing",
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

/// Walk the `node_modules` directory inside a virtual store entry,
/// yielding each package directory (handles scoped packages).
fn walk_package_dirs(node_modules: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let entries = match fs::read_dir(node_modules) {
        Ok(entries) => entries,
        Err(_) => return dirs,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('@') {
            if let Ok(scoped) = fs::read_dir(&path) {
                for scoped_entry in scoped.flatten() {
                    dirs.push(scoped_entry.path());
                }
            }
        } else if name_str != ".bin" {
            dirs.push(path);
        }
    }
    dirs
}

/// Check whether a package directory has lifecycle scripts.
fn has_lifecycle_scripts(pkg_dir: &Path) -> bool {
    let manifest_path = pkg_dir.join("package.json");
    let text = match fs::read_to_string(manifest_path) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let manifest: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(scripts) = manifest.get("scripts").and_then(|v| v.as_object()) else {
        return pkg_dir.join("binding.gyp").exists();
    };
    scripts.contains_key("preinstall")
        || scripts.contains_key("install")
        || scripts.contains_key("postinstall")
        || pkg_dir.join("binding.gyp").exists()
}

/// Compute the package directory inside the virtual store for a snapshot key.
///
/// For a key like `/@pnpm.e2e/pre-and-postinstall-scripts-example@1.0.0`,
/// the directory is:
/// `<virtual_store>/@pnpm.e2e+pre-and-postinstall-scripts-example@1.0.0/node_modules/@pnpm.e2e/pre-and-postinstall-scripts-example`
fn virtual_store_dir_for_key(virtual_store_dir: &Path, key: &PackageKey) -> PathBuf {
    let key_str = key.to_string();
    let name_version = key_str.strip_prefix('/').unwrap_or(&key_str);

    let at_idx = name_version.rfind('@').unwrap_or(name_version.len());
    let name = &name_version[..at_idx];

    let store_name = name_version.replace('/', "+");

    virtual_store_dir.join(&store_name).join("node_modules").join(name)
}
