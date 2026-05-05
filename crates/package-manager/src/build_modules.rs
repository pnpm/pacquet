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

/// Build policy derived from `pnpm.allowBuilds` in the project manifest.
///
/// Ports pnpm's `createAllowBuildFunction` from
/// `https://github.com/pnpm/pnpm/blob/7e91e4b35f/building/policy/src/index.ts`.
///
/// The tri-state return from [`AllowBuildPolicy::check`]:
/// - `Some(true)`: explicitly allowed, run scripts
/// - `Some(false)`: explicitly denied, silently skip
/// - `None`: not in the list, skip and report as ignored
#[derive(Debug, Default)]
pub struct AllowBuildPolicy {
    rules: HashMap<String, bool>,
    policy_present: bool,
}

impl AllowBuildPolicy {
    /// Read `pnpm.allowBuilds` from a project's `package.json`.
    pub fn from_manifest(manifest_dir: &Path) -> Self {
        let manifest_path = manifest_dir.join("package.json");
        let text = match fs::read_to_string(manifest_path) {
            Ok(text) => text,
            Err(_) => return Self::default(),
        };
        let manifest: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };

        let allow_builds =
            manifest.get("pnpm").and_then(|v| v.get("allowBuilds")).and_then(|v| v.as_object());

        let Some(allow_builds) = allow_builds else {
            return Self::default();
        };

        let rules: HashMap<String, bool> = allow_builds
            .iter()
            .filter_map(|(key, value)| value.as_bool().map(|v| (key.clone(), v)))
            .collect();

        Self { rules, policy_present: true }
    }

    /// Check whether a package is allowed to run build scripts.
    ///
    /// `name` is the package name (e.g. `@pnpm.e2e/install-script-example`).
    /// `version` is the resolved version (e.g. `1.0.0`).
    pub fn check(&self, name: &str, version: &str) -> Option<bool> {
        if !self.policy_present {
            return Some(true);
        }

        let exact_key = format!("{name}@{version}");
        if let Some(&allowed) = self.rules.get(&exact_key) {
            return Some(allowed);
        }

        if let Some(&allowed) = self.rules.get(name) {
            return Some(allowed);
        }

        None
    }
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
    pub allow_build_policy: &'a AllowBuildPolicy,
}

impl<'a> BuildModules<'a> {
    pub fn run(self) -> Result<(), BuildModulesError> {
        let BuildModules {
            virtual_store_dir,
            modules_dir,
            lockfile_dir,
            packages,
            snapshots,
            allow_build_policy,
        } = self;

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

            let (name, version) = parse_name_version_from_key(&metadata_key.to_string());
            match allow_build_policy.check(&name, &version) {
                Some(false) => continue,
                None => {
                    tracing::info!(
                        target: "pacquet::build",
                        package = %snapshot_key,
                        "skipping build (not in allowBuilds)",
                    );
                    continue;
                }
                Some(true) => {}
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
    pub allow_build_policy: &'a AllowBuildPolicy,
}

impl<'a> BuildModulesByScanning<'a> {
    pub fn run(self) -> Result<(), BuildModulesError> {
        let BuildModulesByScanning {
            virtual_store_dir,
            modules_dir,
            lockfile_dir,
            allow_build_policy,
        } = self;

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

            let dep_path = store_entry
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            for pkg_entry in walk_package_dirs(&node_modules) {
                if !has_lifecycle_scripts(&pkg_entry) {
                    continue;
                }

                let (name, version) = parse_name_version_from_store_entry(&dep_path);
                match allow_build_policy.check(&name, &version) {
                    Some(false) => continue,
                    None => {
                        tracing::info!(
                            target: "pacquet::build",
                            dep_path,
                            "skipping build (not in allowBuilds)",
                        );
                        continue;
                    }
                    Some(true) => {}
                }

                tracing::info!(
                    target: "pacquet::build",
                    dep_path,
                    dir = %pkg_entry.display(),
                    "running lifecycle scripts (scan)",
                );

                // Warn instead of failing: the scanning path lacks
                // dependency bin linking, so scripts that invoke
                // dependency bins will exit 127. The frozen-lockfile
                // path (BuildModules) propagates errors because it
                // has full lockfile metadata to decide what to build.
                match run_postinstall_hooks(RunPostinstallHooks {
                    dep_path: &dep_path,
                    pkg_root: &pkg_entry,
                    root_modules_dir: modules_dir,
                    init_cwd: lockfile_dir,
                    extra_bin_paths: &extra_bin_paths,
                    extra_env: &extra_env,
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
/// Uses `without_peer()` to strip any peer-dependency suffix before
/// computing the path, so keys like
/// `/@pnpm.e2e/foo@1.0.0(@pnpm.e2e/bar@2.0.0)` resolve correctly.
fn virtual_store_dir_for_key(virtual_store_dir: &Path, key: &PackageKey) -> PathBuf {
    let bare_key = key.without_peer();
    let key_str = bare_key.to_string();
    let name_version = key_str.strip_prefix('/').unwrap_or(&key_str);

    let at_idx = name_version.rfind('@').unwrap_or(name_version.len());
    let name = &name_version[..at_idx];

    let store_name = name_version.replace('/', "+");

    virtual_store_dir.join(&store_name).join("node_modules").join(name)
}

/// Parse `name` and `version` from a lockfile snapshot key like
/// `/@pnpm.e2e/install-script-example@1.0.0`.
fn parse_name_version_from_key(key: &str) -> (String, String) {
    let s = key.strip_prefix('/').unwrap_or(key);
    match s.rfind('@') {
        Some(idx) if idx > 0 => (s[..idx].to_string(), s[idx + 1..].to_string()),
        _ => (s.to_string(), String::new()),
    }
}

/// Parse `name` and `version` from a virtual store entry name like
/// `@pnpm.e2e+install-script-example@1.0.0`.
fn parse_name_version_from_store_entry(entry: &str) -> (String, String) {
    let name_version = match entry.rfind('@') {
        Some(idx) if idx > 0 => (&entry[..idx], &entry[idx + 1..]),
        _ => (entry, ""),
    };
    let name = name_version.0.replacen('+', "/", 1);
    (name, name_version.1.to_string())
}

#[cfg(test)]
mod tests;
