use crate::build_sequence::build_sequence;
use derive_more::{Display, Error};
use miette::Diagnostic;
use pacquet_executor::{LifecycleScriptError, RunPostinstallHooks, run_postinstall_hooks};
use pacquet_lockfile::{PackageKey, PackageMetadata, ProjectSnapshot, SnapshotEntry};
use pacquet_reporter::Reporter;
use std::{
    collections::{BTreeSet, HashMap},
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
    dangerously_allow_all: bool,
}

impl AllowBuildPolicy {
    /// Read `pnpm.allowBuilds` and `pnpm.dangerouslyAllowAllBuilds`
    /// from a project's `package.json`.
    ///
    /// pnpm 11 denies lifecycle scripts by default. Packages must be
    /// explicitly listed in `allowBuilds` to run their scripts.
    pub fn from_manifest(manifest_dir: &Path) -> Self {
        let manifest_path = manifest_dir.join("package.json");
        let text = match fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(
                    target: "pacquet::build",
                    path = %manifest_path.display(),
                    error = %e,
                    "failed to read package.json for build policy",
                );
                return Self::default();
            }
        };
        let manifest: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    target: "pacquet::build",
                    path = %manifest_path.display(),
                    error = %e,
                    "failed to parse package.json for build policy",
                );
                return Self::default();
            }
        };

        let pnpm = manifest.get("pnpm");

        let dangerously_allow_all = pnpm
            .and_then(|v| v.get("dangerouslyAllowAllBuilds"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let allow_builds = pnpm.and_then(|v| v.get("allowBuilds")).and_then(|v| v.as_object());

        let rules: HashMap<String, bool> = allow_builds
            .map(|obj| {
                obj.iter()
                    .filter_map(|(key, value)| value.as_bool().map(|v| (key.clone(), v)))
                    .collect()
            })
            .unwrap_or_default();

        Self { rules, dangerously_allow_all }
    }

    /// Check whether a package is allowed to run build scripts.
    ///
    /// Returns:
    /// - `Some(true)`: explicitly allowed (or `dangerouslyAllowAllBuilds`)
    /// - `Some(false)`: explicitly denied, silently skip
    /// - `None`: not in the list, skip and report as ignored
    pub fn check(&self, name: &str, version: &str) -> Option<bool> {
        if self.dangerously_allow_all {
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
/// `https://github.com/pnpm/pnpm/blob/80037699fb/building/during-install/src/index.ts`.
///
/// Packages are visited in topological order (children before parents) via
/// [`build_sequence`]. Within a chunk, members are independent and could run
/// concurrently — pacquet currently runs them sequentially (TODO: honor
/// `childConcurrency`).
pub struct BuildModules<'a> {
    pub virtual_store_dir: &'a Path,
    pub modules_dir: &'a Path,
    pub lockfile_dir: &'a Path,
    pub packages: Option<&'a HashMap<PackageKey, PackageMetadata>>,
    pub snapshots: Option<&'a HashMap<PackageKey, SnapshotEntry>>,
    pub importers: &'a HashMap<String, ProjectSnapshot>,
    pub allow_build_policy: &'a AllowBuildPolicy,
}

impl<'a> BuildModules<'a> {
    /// Run the build, returning the sorted set of `name@version` keys whose
    /// scripts were skipped because the package was not in `allowBuilds`.
    ///
    /// The caller is expected to fold the returned set into a single
    /// `pnpm:ignored-scripts` event — mirroring upstream's emit at
    /// <https://github.com/pnpm/pnpm/blob/80037699fb/installing/deps-installer/src/install/index.ts#L414>.
    pub fn run<R: Reporter>(self) -> Result<Vec<String>, BuildModulesError> {
        let BuildModules {
            virtual_store_dir,
            modules_dir,
            lockfile_dir,
            packages,
            snapshots,
            importers,
            allow_build_policy,
        } = self;

        let Some(snapshots) = snapshots else { return Ok(Vec::new()) };
        let Some(packages) = packages else { return Ok(Vec::new()) };

        let extra_env = HashMap::new();
        let extra_bin_paths: Vec<PathBuf> = vec![];

        let chunks = build_sequence(packages, snapshots, importers);

        // Collect peer-stripped keys so the final list is unique and
        // sorted lexicographically — matches `dedupePackageNamesFromIgnoredBuilds`.
        let mut ignored_builds: BTreeSet<String> = BTreeSet::new();

        for chunk in chunks {
            for snapshot_key in chunk {
                let metadata_key = snapshot_key.without_peer();
                let Some(metadata) = packages.get(&metadata_key) else { continue };

                // Ancestors-of-build packages are included in the sequence
                // but only run scripts when they themselves require a build.
                if metadata.requires_build != Some(true) {
                    continue;
                }

                let (name, version) = parse_name_version_from_key(&metadata_key.to_string());
                match allow_build_policy.check(&name, &version) {
                    Some(false) => continue,
                    None => {
                        // "Not in allowBuilds" — surfaced as `pnpm:ignored-scripts`.
                        // Explicit `false` is silently skipped (above), matching
                        // upstream's switch in `building/during-install/src/index.ts:88-101`.
                        ignored_builds.insert(metadata_key.to_string());
                        continue;
                    }
                    Some(true) => {}
                }

                let pkg_dir = virtual_store_dir_for_key(virtual_store_dir, &snapshot_key);
                if !pkg_dir.exists() {
                    continue;
                }

                run_postinstall_hooks::<R>(RunPostinstallHooks {
                    dep_path: &snapshot_key.to_string(),
                    pkg_root: &pkg_dir,
                    root_modules_dir: modules_dir,
                    init_cwd: lockfile_dir,
                    extra_bin_paths: &extra_bin_paths,
                    extra_env: &extra_env,
                })
                .map_err(BuildModulesError::LifecycleScript)?;
            }
        }

        Ok(ignored_builds.into_iter().collect())
    }
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

#[cfg(test)]
mod tests;
